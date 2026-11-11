//! Owns only freshly created Claude processes. No process-name termination or shell code.
use pool_core::Result;
use std::{path::{Path,PathBuf},process::{Child,Command,Stdio},time::{Duration,Instant}};
use windows::Win32::{Foundation::{CloseHandle,HANDLE,WAIT_OBJECT_0},System::{
    Console::{AttachConsole,FreeConsole,GenerateConsoleCtrlEvent,GetConsoleWindow,SetConsoleCtrlHandler,CTRL_BREAK_EVENT},
    Diagnostics::ToolHelp::{CreateToolhelp32Snapshot,Thread32First,Thread32Next,TH32CS_SNAPTHREAD,THREADENTRY32},
    JobObjects::{AssignProcessToJobObject,CreateJobObjectW,JobObjectBasicAccountingInformation,JobObjectExtendedLimitInformation,QueryInformationJobObject,SetInformationJobObject,TerminateJobObject,JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,JOBOBJECT_EXTENDED_LIMIT_INFORMATION,JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
    Threading::{OpenThread,ResumeThread,WaitForSingleObject,CREATE_NEW_CONSOLE,CREATE_NEW_PROCESS_GROUP,CREATE_NO_WINDOW,CREATE_SUSPENDED,THREAD_SUSPEND_RESUME},
}};
use std::os::windows::{io::AsRawHandle,process::CommandExt};

struct Handle(HANDLE);
// Each handle is owned, closed exactly once, and is only used under the runtime mutex.
unsafe impl Send for Handle {}
impl Drop for Handle { fn drop(&mut self) { unsafe { let _=CloseHandle(self.0); } } }
pub(super) struct ManagedProcess { child:Child,job:Handle }
impl ManagedProcess {
    pub(super) fn spawn(mut command:Command, hidden:bool)->Result<Self> {
        let job=Handle(unsafe { CreateJobObjectW(None,None) }.map_err(|_|"无法创建 Claude 受管进程组")?);
        let mut info=JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe { SetInformationJobObject(job.0,JobObjectExtendedLimitInformation,(&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),std::mem::size_of_val(&info) as u32) }
            .map_err(|_|"无法设置 Claude 进程组保护")?;
        let flags=CREATE_SUSPENDED.0|CREATE_NEW_PROCESS_GROUP.0|if hidden {CREATE_NO_WINDOW.0}else{CREATE_NEW_CONSOLE.0};
        command.creation_flags(flags);
        if hidden { command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()); }
        let mut child=command.spawn().map_err(|error|format!("无法启动已安装的 Claude Code（系统错误 {}）",error.raw_os_error().unwrap_or(0)))?;
        let result=(|| {
            unsafe { AssignProcessToJobObject(job.0,HANDLE(child.as_raw_handle())) }.map_err(|_|"无法绑定 Claude 受管进程组，未运行客户端")?;
            let snapshot=Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD,0) }.map_err(|_|"无法核验 Claude 启动线程")?);
            let mut entry=THREADENTRY32 {dwSize:std::mem::size_of::<THREADENTRY32>() as u32,..Default::default()};
            let mut ids=Vec::new();
            if unsafe {Thread32First(snapshot.0,&mut entry)}.is_ok() {
                loop {
                    if entry.th32OwnerProcessID==child.id() {ids.push(entry.th32ThreadID);}
                    if unsafe {Thread32Next(snapshot.0,&mut entry)}.is_err(){break;}
                }
            }
            if ids.len()!=1 {return Err("无法唯一核验 Claude 初始线程，未运行客户端".into());}
            let thread=Handle(unsafe {OpenThread(THREAD_SUSPEND_RESUME,false,ids[0])}.map_err(|_|"无法打开 Claude 初始线程")?);
            if unsafe {ResumeThread(thread.0)}==u32::MAX {return Err("无法恢复 Claude 启动线程".into());}
            Ok(())
        })();
        if let Err(error)=result {let _=child.kill();let _=child.wait();return Err(error);}
        Ok(Self {child,job})
    }
    pub(super) fn exited(&self)->Result<bool> {
        let mut info=JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {QueryInformationJobObject(Some(self.job.0),JobObjectBasicAccountingInformation,(&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),std::mem::size_of_val(&info) as u32,None)}
            .map_err(|_|"无法确认 Claude 受管进程组状态")?;
        Ok(info.ActiveProcesses==0)
    }
    pub(super) fn root_exited(&self)->bool { (unsafe{WaitForSingleObject(HANDLE(self.child.as_raw_handle()),0)})==WAIT_OBJECT_0 }
    pub(super) fn terminate(&self)->Result<()> {
        unsafe {TerminateJobObject(self.job.0,1)}.map_err(|_|"无法结束本次 Claude 受管进程组".into())
    }
    pub(super) fn stop(&mut self)->Result<()> {
        if self.exited()? {let _=self.child.wait();return Ok(());}
        // Do not detach an existing developer console or signal unrelated groups.
        if unsafe{GetConsoleWindow()}.is_invalid() && unsafe{AttachConsole(self.child.id())}.is_ok() {
            unsafe {
                let _=SetConsoleCtrlHandler(None,true);
                let _=GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT,self.child.id());
                let _=FreeConsole();
                let _=SetConsoleCtrlHandler(None,false);
            }
        }
        let deadline=Instant::now()+Duration::from_secs(30);
        while !self.exited()? && Instant::now()<deadline {std::thread::sleep(Duration::from_millis(100));}
        if !self.exited()? {self.terminate()?;}
        let deadline=Instant::now()+Duration::from_secs(10);
        while !self.exited()? && Instant::now()<deadline {std::thread::sleep(Duration::from_millis(100));}
        if !self.exited()? {return Err("Claude 受管进程组尚未全部退出".into());}
        let _=self.child.wait();Ok(())
    }
}

pub(super) fn executable()->Result<PathBuf> {
    let mut candidates=Vec::new();
    if let Some(home)=std::env::var_os("USERPROFILE") {candidates.push(PathBuf::from(home).join(".local/bin/claude.exe"));}
    if let Some(paths)=std::env::var_os("PATH") {candidates.extend(std::env::split_paths(&paths).map(|path|path.join("claude.exe")));}
    candidates.into_iter().find(|path|path.is_absolute()&&path.is_file()).and_then(|path|path.canonicalize().ok())
        .ok_or_else(||"未找到原生 Claude Code（claude.exe）；请先按官方方式安装。本版不执行 npm/cmd 包装脚本".into())
}

pub(super) fn command(executable:&Path,base_url:Option<&str>,key:Option<&str>,model:Option<&str>)->Command {
    let mut command=Command::new(executable);
    // Scope to this process; the user's official saved login remains managed by Claude.
    for name in ["ANTHROPIC_API_KEY","ANTHROPIC_AUTH_TOKEN","ANTHROPIC_BASE_URL","CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR","ANTHROPIC_CUSTOM_HEADERS","CLAUDE_CODE_USE_BEDROCK","CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY","CLAUDE_CODE_RETRY_WATCHDOG","CLAUDE_CODE_FALLBACK_MODEL"] {command.env_remove(name);}
    if let Some(url)=base_url {command.env("ANTHROPIC_BASE_URL",url);}
    if let Some(key)=key {command.env("ANTHROPIC_AUTH_TOKEN",key);}
    if let Some(model)=model {
        command.arg("--model").arg(model);
        for name in ["ANTHROPIC_MODEL","ANTHROPIC_DEFAULT_OPUS_MODEL","ANTHROPIC_DEFAULT_SONNET_MODEL","ANTHROPIC_DEFAULT_HAIKU_MODEL","CLAUDE_CODE_SUBAGENT_MODEL"] {command.env(name,model);}
    }
    command.env("CLAUDE_CODE_MAX_RETRIES","0").env("CLAUDE_CODE_NONSTREAMING_TIMEOUT_RETRIES","0")
        .env("CLAUDE_CODE_RESUME_INTERRUPTED_TURN","0").env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC","1")
        .env("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK","1");
    if let Some(base_url)=base_url {
        // CLI-scoped settings override the same user/project keys. Excluding file
        // settings also avoids collecting auth helpers or hidden retry/fallbacks.
        // Machine-managed policy is still honored and checked by the local probe.
        let settings=serde_json::json!({"fallbackModel":[],"env":{
            "ANTHROPIC_BASE_URL":base_url,"CLAUDE_CODE_MAX_RETRIES":"0",
            "CLAUDE_CODE_NONSTREAMING_TIMEOUT_RETRIES":"0","CLAUDE_CODE_RESUME_INTERRUPTED_TURN":"0",
            "CLAUDE_CODE_RETRY_WATCHDOG":"0","CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK":"1",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1"
        }});
        command.args(["--setting-sources",""]).arg("--settings").arg(settings.to_string());
    }
    command
}

pub(super) fn isolate(command:&mut Command,root:&Path) {
    // Read only overrides explicitly supplied by this module, never enumerate
    // the parent's credentials, proxy variables or custom helper configuration.
    let overrides:Vec<_>=command.get_envs().map(|(key,value)|(key.to_os_string(),value.map(|v|v.to_os_string()))).collect();
    command.env_clear();
    for name in ["SystemRoot","WINDIR","PATH","TEMP","TMP","ComSpec","PATHEXT"] {
        if let Some(value)=std::env::var_os(name) {command.env(name,value);}
    }
    for (key,value) in overrides {if let Some(value)=value {command.env(key,value);}else{command.env_remove(key);}}
    command.current_dir(root).env("CLAUDE_CONFIG_DIR",root.join("claude"))
        .env("HOME",root).env("USERPROFILE",root).env("APPDATA",root).env("LOCALAPPDATA",root);
}

pub(super) fn official_login()->Result<()> {
    let exe=executable()?;
    Command::new(exe).args(["auth","login"]).creation_flags(CREATE_NEW_CONSOLE.0|CREATE_NEW_PROCESS_GROUP.0)
        .spawn().map_err(|_|"无法打开官方 Claude Code 登录流程")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_values_are_arguments_and_local_environment_not_shell_code() {
        let command=command(Path::new("C:\\fixture\\claude.exe"),Some("http://127.0.0.1:1/fusion-api/fixture"),Some("public-fixture-local-key"),Some("model;$(unsafe)"));
        assert_eq!(command.get_program(),"C:\\fixture\\claude.exe");
        let args:Vec<_>=command.get_args().collect();assert_eq!(&args[..2],&["--model","model;$(unsafe)"]);
        assert_eq!(&args[2..4],&["--setting-sources",""]);
        assert!(!args.last().unwrap().to_string_lossy().contains("public-fixture-local-key"));
        let env:Vec<_>=command.get_envs().collect();
        assert!(env.iter().any(|(key,value)|*key=="CLAUDE_CODE_MAX_RETRIES"&&*value==Some(std::ffi::OsStr::new("0"))));
        assert!(env.iter().any(|(key,value)|*key=="CLAUDE_CODE_OAUTH_TOKEN"&&value.is_none()));
        let subscription=super::command(Path::new("C:\\fixture\\claude.exe"),Some("http://127.0.0.1:1/fusion-pool"),Some("public-fixture-key"),None);
        assert!(!subscription.get_args().any(|a|a=="--model"));
        assert!(!subscription.get_envs().any(|(key,value)|key.to_string_lossy().starts_with("ANTHROPIC_DEFAULT_")&&value.is_some()));
    }
    #[test]
    fn hidden_owned_job_starts_suspended_and_reaps_its_own_process() {
        let root=tempfile::tempdir().unwrap();
        let cmd=PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        let mut command=Command::new(cmd);command.args(["/d","/c","exit","0"]).current_dir(root.path());
        let process=ManagedProcess::spawn(command,true).unwrap();
        let deadline=Instant::now()+Duration::from_secs(5);
        while !process.exited().unwrap()&&Instant::now()<deadline {std::thread::sleep(Duration::from_millis(20));}
        assert!(process.exited().unwrap());
    }
}
