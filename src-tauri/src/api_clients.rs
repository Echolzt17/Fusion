//! Independent managed API connections; the existing subscription Connection stays intact.
use pool_core::{api_config,api_gateway::{ApiGateway,ApiRequestLog},api_sources::{ClientKind,Protocol,Sources},storage::Store,Result};
use serde::{Deserialize,Serialize};
use std::{path::PathBuf,sync::{Arc,Mutex}};
#[cfg(windows)]
mod process;
#[cfg(windows)]
mod probe;
#[cfg(test)]
mod tests;

#[derive(Clone,Serialize)]
#[serde(rename_all="camelCase")]
pub struct ClientStatus {
    pub client:ClientKind,
    pub mode:String,
    pub phase:String,
    pub source_id:Option<String>,
    pub source_name:Option<String>,
    pub source_revision:Option<u64>,
    pub error:Option<String>,
    pub active_requests:usize,
    pub desktop_acceptance:&'static str,
    pub retry_verified:bool,
    pub history_incomplete:bool,
}
impl ClientStatus {
    fn off(client:ClientKind)->Self {Self {client,mode:"off".into(),phase:"off".into(),source_id:None,source_name:None,source_revision:None,error:None,active_requests:0,desktop_acceptance:"pending",retry_verified:false,history_incomplete:false}}
}
#[derive(Serialize,Deserialize)]
struct Pending {source_id:String,source_name:String,#[serde(default)] source_revision:Option<u64>}
struct Runtime {
    gateway:Option<Arc<ApiGateway>>,
    #[cfg(windows)]
    process:Option<process::ManagedProcess>,
}
struct Slot {operation:tokio::sync::Mutex<()>,runtime:tokio::sync::Mutex<Runtime>,status:Mutex<ClientStatus>}
pub struct ApiClients {store:Arc<Store>,sources:Arc<Sources>,path:PathBuf,codex:Slot,claude:Slot}
fn state_key(client:ClientKind)->&'static str {match client {ClientKind::Codex=>"api-connections/v1/codex-state",ClientKind::ClaudeCode=>"api-connections/v1/claude-state"}}
impl ApiClients {
    pub fn new(store:Arc<Store>,sources:Arc<Sources>,path:PathBuf)->Arc<Self> {
        let slot=|client| {
            let mut status=ClientStatus::off(client);
            match store.get(state_key(client)) {
                Ok(Some(raw)) if !raw.is_empty()=>{
                    status.mode="recoveryRequired".into();status.phase="recoveryRequired".into();
                    match serde_json::from_str::<Pending>(&raw) {
                        Ok(pending)=>{status.source_id=Some(pending.source_id);status.source_name=Some(pending.source_name);status.source_revision=pending.source_revision;status.error=Some("上次接入未完成正常关闭，请先关闭并恢复；不会自动重新启用".into());},
                        Err(_)=>status.error=Some("API 接入状态损坏，请保留数据并修复恢复记录".into()),
                    }
                },
                Err(error)=>{status.mode="recoveryRequired".into();status.phase="recoveryRequired".into();status.error=Some(error);},
                _=>{},
            }
            if client==ClientKind::Codex && api_config::pending(&store).unwrap_or(true) {status.mode="recoveryRequired".into();status.phase="recoveryRequired".into();}
            Slot {operation:tokio::sync::Mutex::new(()),runtime:tokio::sync::Mutex::new(Runtime {gateway:None,#[cfg(windows)] process:None}),status:Mutex::new(status)}
        };
        let codex=slot(ClientKind::Codex);let claude=slot(ClientKind::ClaudeCode);
        Arc::new(Self {store,sources,path,codex,claude})
    }
    fn slot(&self,client:ClientKind)->&Slot {match client {ClientKind::Codex=>&self.codex,ClientKind::ClaudeCode=>&self.claude}}
    fn update(&self,client:ClientKind,f:impl FnOnce(&mut ClientStatus)) {f(&mut self.slot(client).status.lock().unwrap_or_else(|error|error.into_inner()));}
    pub async fn statuses(&self)->Result<Vec<ClientStatus>> {
        let mut statuses=Vec::new();
        for client in [ClientKind::Codex,ClientKind::ClaudeCode] {
            let slot=self.slot(client);let mut status=slot.status.lock().map_err(|_|"API 客户端状态锁异常")?.clone();
            if let Ok(runtime)=slot.runtime.try_lock() {
                if let Some(gateway)=&runtime.gateway {let gateway=gateway.status();status.active_requests=gateway.active_requests;status.history_incomplete=gateway.history_incomplete;
                    if !gateway.running && status.mode=="running" {status.mode="recoveryRequired".into();status.error=Some("本机 API 网关已停止，请关闭并恢复此接入".into());}}
                #[cfg(windows)]
                if runtime.process.as_ref().is_some_and(|p|p.root_exited()) && status.mode=="running" {
                    status.mode="recoveryRequired".into();status.error=Some("受管 Claude Code 已退出，请关闭此连接后重新启动".into());
                }
            }
            statuses.push(status);
        }
        Ok(statuses)
    }
    pub async fn has_connection(&self,client:ClientKind)->Result<bool> {
        let state=self.slot(client).status.lock().map_err(|_|"API 客户端状态锁异常")?;
        Ok(state.mode!="off" || self.store.get(state_key(client))?.is_some_and(|v|!v.is_empty())
            || (client==ClientKind::Codex&&api_config::pending(&self.store)?))
    }
    pub async fn ensure_source_idle(&self,id:&str)->Result<()> {
        for slot in [&self.codex,&self.claude] {
            let status=slot.status.lock().map_err(|_|"API 客户端状态锁异常")?;
            if status.mode!="off" && (status.source_id.is_none()||status.source_id.as_deref()==Some(id)) {return Err("此来源仍被启动、运行或待恢复的连接引用；恢复记录无法确认来源时也不能修改，请先关闭对应连接".into());}
        }
        Ok(())
    }
    pub async fn logs(&self,client:ClientKind)->Result<Vec<ApiRequestLog>> {
        self.sources.logs(client)
    }
    pub async fn start(&self,client:ClientKind,id:&str)->Result<()> {
        let slot=self.slot(client);let _operation=slot.operation.try_lock().map_err(|_|"此客户端正在切换，请等待完成")?;
        let source=self.sources.get(id)?;
        if !source.enabled || !source.has_key {return Err("API 来源已停用或缺少密钥".into());}
        if !matches!((client,source.protocol),(ClientKind::Codex,Protocol::Responses)|(ClientKind::ClaudeCode,Protocol::Messages)) {
            return Err("来源协议与客户端不兼容".into());
        }
        self.stop_locked(client).await?;
        self.store.set(state_key(client),&serde_json::to_string(&Pending {source_id:source.id.clone(),source_name:source.name.clone(),source_revision:Some(source.revision)}).map_err(|_|"API 状态编码失败")?)?;
        self.update(client,|s|{s.mode="starting".into();s.phase="checking".into();s.source_id=Some(source.id.clone());s.source_name=Some(source.name.clone());s.source_revision=Some(source.revision);s.error=None;});
        let result=self.start_inner(client,&source).await;
        match &result {
            Ok(())=>self.update(client,|s|{s.mode="running".into();s.phase="running".into();}),
            Err(error)=>self.update(client,|s|{s.mode="recoveryRequired".into();s.phase="failed".into();s.error=Some(error.clone());}),
        }
        result
    }
    async fn start_inner(&self,client:ClientKind,source:&pool_core::api_sources::ApiSource)->Result<()> {
        match client {
            ClientKind::Codex=>{
                let store=self.store.clone();
                let identity=crate::blocking(move||crate::codex_window::prepare_for_enable(&store)).await?;
                let gateway=ApiGateway::start(self.sources.clone(),client,&source.id).await?;
                let (url,key)=(gateway.endpoint().base_url().to_owned(),gateway.endpoint().local_key().to_owned());
                self.slot(client).runtime.lock().await.gateway=Some(gateway);
                self.update(client,|s|s.phase="configuring".into());
                let (store,path,id,revision,model)=(self.store.clone(),self.path.clone(),source.id.clone(),source.revision,source.model.clone());
                crate::blocking(move||api_config::apply(&store,&path,&id,revision,&model,&url,&key)).await?;
                self.update(client,|s|s.phase="stoppingClient".into());
                let store=self.store.clone();let target=crate::blocking(move||crate::codex_lifecycle::stop_client(&store,|_|{})).await?;
                self.update(client,|s|s.phase="launching".into());
                let store=self.store.clone();crate::blocking(move||crate::codex_lifecycle::launch(target,&identity,&store,|_|{})).await?;
                api_config::verify_applied(&self.store,&self.path)?;
                self.store.finish_client_switch(false)?;
                self.update(client,|s|s.retry_verified=true);
            },
            ClientKind::ClaudeCode=>{
                #[cfg(windows)]
                {
                    let exe=process::executable()?;
                    self.update(client,|s|s.phase="verifyingRetries".into());
                    probe::verify(&exe,&source.model).await?;
                    self.update(client,|s|{s.phase="launching".into();s.retry_verified=true;});
                    let gateway=ApiGateway::start(self.sources.clone(),client,&source.id).await?;
                    let base=gateway.endpoint().base_url().strip_suffix("/v1").ok_or("Claude 本机接口路径无效")?;
                    let command=process::command(&exe,Some(base),Some(gateway.endpoint().local_key()),Some(&source.model));
                    let runtime=&mut *self.slot(client).runtime.lock().await;
                    runtime.gateway=Some(gateway);
                    runtime.process=Some(process::ManagedProcess::spawn(command,false)?);
                }
                #[cfg(not(windows))]
                return Err("Claude 受管终端仅支持 Windows".into());
            }
        }
        Ok(())
    }
    pub async fn stop(&self,client:ClientKind)->Result<()> {
        let _operation=self.slot(client).operation.try_lock().map_err(|_|"此客户端正在切换，请等待完成")?;
        self.stop_locked(client).await
    }
    async fn stop_locked(&self,client:ClientKind)->Result<()> {
        if !self.has_connection(client).await? {return Ok(());}
        self.update(client,|s|{s.mode="stopping".into();s.phase="restoring".into();s.error=None;});
        let result=self.stop_inner(client).await;
        if let Err(error)=&result {self.update(client,|s|{s.mode="recoveryRequired".into();s.phase="failed".into();s.error=Some(error.clone());});}
        result
    }
    async fn stop_inner(&self,client:ClientKind)->Result<()> {
        let slot=self.slot(client);
        let must_stop=if client==ClientKind::Codex {
            let (store,path)=(self.store.clone(),self.path.clone());crate::blocking(move||api_config::restore(&store,&path)).await?
        } else {false};
        let gateway=slot.runtime.lock().await.gateway.clone();
        if let Some(gateway)=&gateway {
            self.update(client,|s|s.phase="draining".into());gateway.begin_drain();gateway.wait_drained().await;
        }
        self.update(client,|s|s.phase="stoppingClient".into());
        if must_stop {
            let store=self.store.clone();crate::blocking(move||crate::codex_lifecycle::stop_client(&store,|_|{}).map(|_|())).await?;
            api_config::finish_close(&self.store,&self.path)?;self.store.finish_client_switch(false)?;
        }
        let mut runtime=slot.runtime.lock().await;
        #[cfg(windows)]
        if let Some(mut process)=runtime.process.take() {
            let (result,process)=tokio::task::spawn_blocking(move||{let result=process.stop();(result,process)}).await.map_err(|_|"Claude 进程关闭任务异常")?;
            if let Err(error)=result {runtime.process=Some(process);return Err(error);}
        }
        if let Some(gateway)=runtime.gateway.take() {
            gateway.stop().await;
        }
        self.store.set(state_key(client),"")?;
        self.update(client,|s|*s=ClientStatus::off(client));Ok(())
    }
    pub async fn stop_all(&self)->Result<()> {
        // Attempt both independent connections, even if one needs manual recovery.
        let (codex,claude)=tokio::join!(self.stop(ClientKind::Codex),self.stop(ClientKind::ClaudeCode));
        codex.and(claude)
    }
    pub async fn official_claude(&self)->Result<()> {
        if self.has_connection(ClientKind::ClaudeCode).await? {return Err("请先关闭 Claude API 连接，再打开官方登录流程".into());}
        #[cfg(windows)]
        return crate::blocking(process::official_login).await;
        #[cfg(not(windows))]
        Err("官方 Claude Code 登录入口仅支持 Windows".into())
    }
}
