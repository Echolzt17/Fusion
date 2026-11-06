use super::*;
use std::os::windows::ffi::OsStrExt;
use windows::{core::{PCWSTR, PWSTR}, Win32::{
    Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0},
    Security::{Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW}, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::FileSystem::{CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::{JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject}, Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject}},
}};

struct OwnedHandle(HANDLE);
// Each handle has one owner. Mutable process state is protected by NetworkKernel's mutex.
unsafe impl Send for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) { unsafe { let _ = CloseHandle(self.0); } }
}

struct ProcessAttributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _buffer: Vec<usize>,
    _handles: Vec<HANDLE>,
}
impl ProcessAttributes {
    fn inherited_handles(handles: &[HANDLE]) -> Result<Self> {
        let mut bytes = 0;
        unsafe { let _ = InitializeProcThreadAttributeList(None, 1, None, &mut bytes); }
        if bytes == 0 || bytes > 64 * 1024 { return Err("代理内核句柄隔离信息无效".into()); }
        let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes) }.map_err(|_| "无法准备代理内核句柄隔离")?;
        let attributes = Self { list, _buffer: buffer, _handles: handles.to_vec() };
        unsafe { UpdateProcThreadAttribute(attributes.list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, Some(attributes._handles.as_ptr().cast()), std::mem::size_of_val(attributes._handles.as_slice()), None, None) }.map_err(|_| "无法限制代理内核继承句柄")?;
        Ok(attributes)
    }
}
impl Drop for ProcessAttributes {
    fn drop(&mut self) { unsafe { DeleteProcThreadAttributeList(self.list); } }
}

fn wide(path: &Path) -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() }

fn current_user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|_| "无法核验代理内核运行用户")?;
    let token = OwnedHandle(token);
    let mut required = 0;
    unsafe { let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut required); }
    if required == 0 || required > 64 * 1024 { return Err("代理内核运行用户信息无效".into()); }
    // TOKEN_USER contains pointers and must be pointer-aligned.
    let mut buffer = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe { GetTokenInformation(token.0, TokenUser, Some(buffer.as_mut_ptr().cast()), required, &mut required) }.map_err(|_| "无法读取代理内核运行用户")?;
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid) }.map_err(|_| "无法准备代理内核目录权限")?;
    let result = unsafe { sid.to_string() }.map_err(|_| "代理内核运行用户编码无效");
    unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))); }
    result.map_err(Into::into)
}

pub(super) fn create_private_directory(path: &Path) -> Result<()> {
    check_no_links(path.parent().ok_or("代理内核目录无效")?)?;
    let sid = current_user_sid()?;
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})").encode_utf16().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), 1, &mut descriptor, None) }.map_err(|_| "无法生成代理内核目录权限")?;
    let security = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into() };
    let target = wide(path);
    let created = unsafe { CreateDirectoryW(PCWSTR(target.as_ptr()), Some(&security)) };
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))); }
    created.map_err(|_| "无法创建受保护的代理内核目录".into())
}

pub(super) struct OwnedProcess {
    process: OwnedHandle,
    job: Option<OwnedHandle>,
    runtime: PathBuf,
    runtime_root: PathBuf,
}

impl OwnedProcess {
    #[cfg(test)]
    pub fn id(&self) -> u32 {
        unsafe { windows::Win32::System::Threading::GetProcessId(self.process.0) }
    }

    pub fn spawn(executable: &Path, runtime: &Path, runtime_root: &Path) -> Result<Self> {
        let job = OwnedHandle(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| "无法创建代理内核进程隔离")?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe { SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of_val(&limits) as u32) }.map_err(|_| "无法设置代理内核退出保护")?;
        let nul = wide(Path::new("NUL"));
        let inherit = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, bInheritHandle: true.into(), ..Default::default() };
        let null = OwnedHandle(unsafe { CreateFileW(PCWSTR(nul.as_ptr()), FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0, FILE_SHARE_READ | FILE_SHARE_WRITE, Some(&inherit), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) }.map_err(|_| "无法隔离代理内核输出")?);
        let executable_wide = wide(executable);
        let working_directory = wide(runtime);
        // Windows file names cannot contain quotes. These are verified local paths,
        // with no secret, node name, subscription address, or shell interpretation.
        let command = format!("\"{}\" -d \"{}\" -f \"{}\"", executable.display(), runtime.display(), runtime.join("config.yaml").display());
        let mut command_wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        // bInheritHandles must be true for redirected standard handles, but the
        // explicit list keeps unrelated API pipes and other inheritable handles
        // out of this process. Only the NUL handle is inherited.
        let attributes = ProcessAttributes::inherited_handles(&[null.0])?;
        let startup = STARTUPINFOEXW { StartupInfo: STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOEXW>() as u32, dwFlags: STARTF_USESTDHANDLES, hStdInput: null.0, hStdOutput: null.0, hStdError: null.0, ..Default::default() }, lpAttributeList: attributes.list };
        let mut info = PROCESS_INFORMATION::default();
        unsafe { CreateProcessW(PCWSTR(executable_wide.as_ptr()), Some(PWSTR(command_wide.as_mut_ptr())), None, None, true, CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT, None, PCWSTR(working_directory.as_ptr()), &startup.StartupInfo, &mut info) }.map_err(|_| "无法启动内置代理内核")?;
        let process = OwnedHandle(info.hProcess);
        let thread = OwnedHandle(info.hThread);
        // The child cannot execute any code before entering the kill-on-close job.
        // This closes the spawn/assignment orphan window on parent failure.
        if unsafe { AssignProcessToJobObject(job.0, process.0) }.is_err() {
            unsafe { let _ = TerminateProcess(process.0, 1); WaitForSingleObject(process.0, 5000); }
            return Err("无法将代理内核加入受管进程组".into());
        }
        if unsafe { ResumeThread(thread.0) } == u32::MAX { return Err("无法运行受管代理内核".into()); }
        Ok(Self { process, job: Some(job), runtime: runtime.to_owned(), runtime_root: runtime_root.to_owned() })
    }

    pub fn running(&self) -> bool { unsafe { WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 } }

    pub fn stop(&mut self) -> Result<()> {
        // The caller has already drained logical requests. Closing this specific
        // owned job affects only its verified kernel and any of its descendants.
        self.job.take();
        if unsafe { WaitForSingleObject(self.process.0, 5000) } != WAIT_OBJECT_0 {
            return Err("代理内核尚未完全退出，请重试关闭".into());
        }
        remove_owned_runtime(&self.runtime_root, &self.runtime)
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) { let _ = self.stop(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::{Authorization::{ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SE_FILE_OBJECT}, DACL_SECURITY_INFORMATION};

    #[test]
    fn directory_acl_replaces_inherited_permissions_with_user_and_system() {
        let temporary = tempfile::tempdir().unwrap();
        let private = temporary.path().join("private");
        create_private_directory(&private).unwrap();
        let file = private.join("config.yaml");
        std::fs::write(&file, "public fixture").unwrap();
        let file_wide = wide(&file);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe { GetNamedSecurityInfoW(PCWSTR(file_wide.as_ptr()), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, None, None, &mut descriptor) }.ok().unwrap();
        let mut sddl = PWSTR::null();
        unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor, 1, DACL_SECURITY_INFORMATION, &mut sddl, None) }.unwrap();
        let value = unsafe { sddl.to_string() }.unwrap();
        let sid = current_user_sid().unwrap();
        assert!(value.contains(&sid));
        assert!(value.contains(";;;SY)"));
        assert!(!value.contains(";;;BU)") && !value.contains(";;;AU)") && !value.contains(";;;WD)"));
        unsafe { LocalFree(Some(HLOCAL(sddl.0.cast()))); LocalFree(Some(HLOCAL(descriptor.0))); }
    }

    #[test]
    fn unrelated_inheritable_handles_do_not_reach_the_kernel() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("private-channel-fixture");
        std::fs::write(&file, b"public test fixture").unwrap();
        let file_wide = wide(&file);
        let inherit = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, bInheritHandle: true.into(), ..Default::default() };
        // A leaked handle with no sharing would keep this file locked while the
        // kernel is running, even after the parent closes its own handle.
        let exclusive = OwnedHandle(unsafe { CreateFileW(PCWSTR(file_wide.as_ptr()), FILE_GENERIC_READ.0, Default::default(), Some(&inherit), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) }.unwrap());
        let kernel = NetworkKernel::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/network-kernel/mihomo.exe"), temporary.path().join("runtime")).unwrap();
        kernel.start("secret: fixture-control-secret-01234567890123456789\nlisteners:\n- name: fusion-http\n  type: http\n  users:\n  - username: fixture-route\n    password: fixture-password\nrules:\n- MATCH,REJECT\n").unwrap();
        drop(exclusive);
        let removed = std::fs::remove_file(&file);
        kernel.stop().unwrap();
        assert!(removed.is_ok(), "kernel inherited an unrelated parent handle");
    }
}
