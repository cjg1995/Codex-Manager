use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::process::Command;

#[cfg(windows)]
use sysinfo::{ProcessRefreshKind, System, UpdateKind};

#[cfg(windows)]
const STORE_APP_ID: &str = "App";
#[cfg(windows)]
const CLOSE_GRACE_PERIOD: Duration = Duration::from_secs(8);
#[cfg(windows)]
const CLOSE_FORCE_PERIOD: Duration = Duration::from_secs(5);
#[cfg(windows)]
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const IDENTITY_STABLE_PERIOD: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedProcess {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub cmd: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexProcessKind {
    Desktop,
    AppServer,
    UnknownChatGpt,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopInstall {
    pub executable: Option<PathBuf>,
    pub package_family_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexAuthIdentity {
    pub account_id: String,
    pub chatgpt_account_id: Option<String>,
}

pub(crate) fn paths_match(left: &Path, right: &Path) -> bool {
    fn key(path: &Path) -> String {
        let candidate = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let text = candidate.to_string_lossy().replace('/', "\\");
        let without_prefix = text.strip_prefix(r"\\?\").unwrap_or(&text);
        without_prefix.trim_end_matches('\\').to_ascii_lowercase()
    }
    key(left) == key(right)
}

pub(crate) fn classify_process(process: &ObservedProcess) -> CodexProcessKind {
    let name = process.name.to_ascii_lowercase();
    if process_name_is(&name, "chatgpt") {
        return classify_chatgpt(process);
    }
    if process_name_is(&name, "codex") {
        if process
            .exe
            .as_deref()
            .is_some_and(is_codex_desktop_executable)
        {
            return CodexProcessKind::Desktop;
        }
        if is_official_app_server(process) {
            return CodexProcessKind::AppServer;
        }
    }
    CodexProcessKind::Ignore
}

pub(crate) fn processes_to_stop(processes: &[ObservedProcess]) -> Result<Vec<u32>, String> {
    let mut unknown = Vec::new();
    let mut roots = Vec::new();
    for process in processes {
        match classify_process(process) {
            CodexProcessKind::UnknownChatGpt => unknown.push(process.pid),
            CodexProcessKind::Desktop | CodexProcessKind::AppServer => roots.push(process.pid),
            CodexProcessKind::Ignore => {}
        }
    }
    if !unknown.is_empty() {
        let ids = unknown
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "发现无法确认归属的 ChatGPT.exe（PID {ids}）。为避免关掉其他程序，已取消切换。"
        ));
    }
    let mut manager_pids = vec![std::process::id()];
    manager_pids.extend(processes.iter().filter_map(|process| {
        let name = process.name.to_ascii_lowercase();
        (process_name_is(&name, "codexmanager") || process_name_is(&name, "codexmanager-service"))
            .then_some(process.pid)
    }));
    Ok(descendant_pids(processes, &roots, &manager_pids))
}

pub(crate) fn select_running_install(processes: &[ObservedProcess]) -> Option<DesktopInstall> {
    let mut desktop = processes
        .iter()
        .filter(|process| classify_process(process) == CodexProcessKind::Desktop)
        .collect::<Vec<_>>();
    desktop.sort_by_key(|process| command_is_helper(&process.cmd));
    let exe = desktop.first()?.exe.as_deref()?;
    install_from_path(exe)
}

pub(crate) fn package_family_from_dir_name(dir_name: &str) -> Option<String> {
    let (left, publisher) = dir_name.rsplit_once("__")?;
    if publisher.is_empty() {
        return None;
    }
    let (name_and_version, _arch) = left.rsplit_once('_')?;
    let (name, version) = name_and_version.rsplit_once('_')?;
    if !version.chars().next()?.is_ascii_digit() {
        return None;
    }
    if !name.to_ascii_lowercase().starts_with("openai.codex") {
        return None;
    }
    Some(format!("{name}_{publisher}"))
}

pub(crate) fn package_family_from_executable(exe: &Path) -> Option<String> {
    exe.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(package_family_from_dir_name)
    })
}

pub(crate) fn install_from_path(exe: &Path) -> Option<DesktopInstall> {
    if !is_codex_desktop_executable(exe) {
        return None;
    }
    Some(DesktopInstall {
        executable: Some(exe.to_path_buf()),
        package_family_name: package_family_from_executable(exe),
    })
}

pub(crate) fn identity_from_auth_json(content: &str) -> Result<CodexAuthIdentity, String> {
    let value: Value =
        serde_json::from_str(content).map_err(|err| format!("解析 auth.json 失败：{err}"))?;
    let tokens = value
        .get("tokens")
        .and_then(Value::as_object)
        .ok_or_else(|| "auth.json 缺少 tokens".to_string())?;
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "auth.json 缺少账号标识".to_string())?;
    let chatgpt_account_id = ["access_token", "id_token"].iter().find_map(|key| {
        tokens
            .get(*key)
            .and_then(Value::as_str)
            .and_then(codexmanager_core::auth::extract_chatgpt_account_id)
    });
    Ok(CodexAuthIdentity {
        account_id: account_id.to_string(),
        chatgpt_account_id,
    })
}

pub(crate) fn read_auth_identity(path: &Path) -> Result<CodexAuthIdentity, String> {
    let content = fs::read_to_string(path)
        .map_err(|err| format!("读取 auth.json 失败（{}）：{err}", path.display()))?;
    identity_from_auth_json(&content)
}

pub(crate) fn auth_identity_matches(
    expected: &CodexAuthIdentity,
    actual: &CodexAuthIdentity,
) -> bool {
    if expected.account_id != actual.account_id {
        return false;
    }
    match (
        expected.chatgpt_account_id.as_deref(),
        actual.chatgpt_account_id.as_deref(),
    ) {
        (Some(expected_id), Some(actual_id)) => expected_id == actual_id,
        _ => true,
    }
}

pub(crate) fn close_running_codex() -> Result<DesktopInstall, String> {
    #[cfg(windows)]
    {
        close_running_codex_windows()
    }
    #[cfg(not(windows))]
    {
        Err("当前系统还不能在切换账号后自动打开 Codex，账号未切换。".to_string())
    }
}

pub(crate) fn launch(install: &DesktopInstall) -> Result<u32, String> {
    #[cfg(windows)]
    {
        launch_windows(install)
    }
    #[cfg(not(windows))]
    {
        let _ = install;
        Err("当前系统还不能在切换账号后自动打开 Codex。".to_string())
    }
}

pub(crate) fn confirm_launch(
    pid: u32,
    path: &Path,
    expected: &CodexAuthIdentity,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        wait_for_window_and_auth(
            path,
            expected,
            LAUNCH_TIMEOUT,
            IDENTITY_STABLE_PERIOD,
            || has_main_window(pid),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, path, expected);
        Err("当前系统还不能确认 Codex 是否已经打开。".to_string())
    }
}

#[cfg(any(windows, test))]
fn wait_for_window_and_auth(
    path: &Path,
    expected: &CodexAuthIdentity,
    timeout: Duration,
    stable_for: Duration,
    mut has_window: impl FnMut() -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut matched_at = None;
    loop {
        let error = if has_window() {
            match read_auth_identity(path) {
                Ok(actual) if auth_identity_matches(expected, &actual) => {
                    let started = matched_at.get_or_insert_with(Instant::now);
                    if started.elapsed() >= stable_for {
                        return Ok(());
                    }
                    "Codex 已打开，登录信息尚未稳定。"
                }
                Ok(_) => {
                    matched_at = None;
                    "Codex 已打开，登录信息与所选账号不一致。"
                }
                Err(_) => {
                    matched_at = None;
                    "Codex 已打开，但未能读取登录信息。"
                }
            }
        } else {
            matched_at = None;
            "已写入所选账号的配置，但未检测到 Codex 窗口。"
        };
        if Instant::now() >= deadline {
            return Err(error.to_string());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn classify_chatgpt(process: &ObservedProcess) -> CodexProcessKind {
    let Some(exe) = process.exe.as_deref() else {
        return CodexProcessKind::UnknownChatGpt;
    };
    if is_classic_chatgpt(exe) {
        return CodexProcessKind::Ignore;
    }
    if is_store_codex_desktop(exe) {
        return CodexProcessKind::Desktop;
    }
    if is_local_codex_desktop(exe) && desktop_resources_exist(exe) {
        return CodexProcessKind::Desktop;
    }
    CodexProcessKind::UnknownChatGpt
}

fn is_codex_desktop_executable(path: &Path) -> bool {
    if is_classic_chatgpt(path) {
        return false;
    }
    if is_store_codex_desktop(path) {
        return true;
    }
    is_local_codex_desktop(path) && desktop_resources_exist(path)
}

fn is_store_codex_desktop(path: &Path) -> bool {
    let name = file_name_lower(path);
    if name != "chatgpt.exe" && name != "codex.exe" {
        return false;
    }
    let lower = normalized_path(path);
    if is_classic_chatgpt_text(&lower) {
        return false;
    }
    lower.contains("\\windowsapps\\openai.codex") && lower.contains("\\app\\")
}

fn is_local_codex_desktop(path: &Path) -> bool {
    let lower = normalized_path(path);
    if lower.contains("\\openai\\codex\\") {
        return false;
    }
    lower.ends_with("\\programs\\chatgpt\\chatgpt.exe")
        || lower.ends_with("\\programs\\codex\\codex.exe")
}

fn is_classic_chatgpt(path: &Path) -> bool {
    is_classic_chatgpt_text(&normalized_path(path))
}

fn is_classic_chatgpt_text(lower: &str) -> bool {
    lower.contains("chatgpt-desktop") || lower.contains("openai.chatgpt")
}

fn desktop_resources_exist(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    dir.join("resources").join("codex.exe").is_file()
        || dir.join("resources").join("codex").is_file()
}

fn is_official_app_server(process: &ObservedProcess) -> bool {
    if !process.cmd.iter().any(|arg| arg == "app-server") {
        return false;
    }
    process.exe.as_deref().is_some_and(is_official_codex_binary)
}

fn is_official_codex_binary(path: &Path) -> bool {
    let lower = normalized_path(path);
    lower.contains("\\openai\\codex\\bin\\")
        || lower.contains("\\openai.codex")
        || lower.ends_with("\\resources\\codex.exe")
}

fn command_is_helper(cmd: &[String]) -> bool {
    cmd.iter()
        .any(|arg| arg == "--type" || arg.starts_with("--type="))
}

fn process_name_is(actual: &str, expected_stem: &str) -> bool {
    actual == expected_stem || actual == format!("{expected_stem}.exe")
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn descendant_pids(
    processes: &[ObservedProcess],
    roots: &[u32],
    excluded_pids: &[u32],
) -> Vec<u32> {
    let mut children = std::collections::HashMap::<u32, Vec<u32>>::new();
    for process in processes {
        if let Some(parent) = process.parent_pid {
            children.entry(parent).or_default().push(process.pid);
        }
    }
    let mut ordered = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if pid == 0 || excluded_pids.contains(&pid) || !seen.insert(pid) {
            continue;
        }
        ordered.push(pid);
        if let Some(child_pids) = children.get(&pid) {
            stack.extend(child_pids.iter().copied());
        }
    }
    ordered
}

#[cfg(windows)]
fn close_running_codex_windows() -> Result<DesktopInstall, String> {
    let processes = snapshot_processes();
    let pids = processes_to_stop(&processes)?;
    let install = select_running_install(&processes)
        .or_else(|| discover_installed().ok())
        .ok_or_else(|| "找不到 Codex 桌面程序，账号未切换。".to_string())?;
    if pids.is_empty() {
        return Ok(install);
    }
    request_stop(&pids, false)?;
    if poll_until_stopped(CLOSE_GRACE_PERIOD)?.is_none() {
        let remaining = processes_to_stop(&snapshot_processes())?;
        request_stop(&remaining, true)?;
        if poll_until_stopped(CLOSE_FORCE_PERIOD)?.is_none() {
            return Err("Codex 仍在运行，账号未切换。".to_string());
        }
    }
    Ok(install)
}

#[cfg(windows)]
fn poll_until_stopped(timeout: Duration) -> Result<Option<()>, String> {
    let started = Instant::now();
    loop {
        let processes = snapshot_processes();
        if processes_to_stop(&processes)?.is_empty() {
            return Ok(Some(()));
        }
        if started.elapsed() >= timeout {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(windows)]
fn request_stop(pids: &[u32], force: bool) -> Result<(), String> {
    let mut args = Vec::new();
    for pid in pids {
        args.push("/PID".to_string());
        args.push(pid.to_string());
    }
    if args.is_empty() {
        return Ok(());
    }
    // The target list already follows the process tree and excludes the manager.
    if force {
        args.push("/F".to_string());
    }
    run_hidden("taskkill.exe", &args)
}

#[cfg(windows)]
fn launch_windows(install: &DesktopInstall) -> Result<u32, String> {
    if let Some(family) = install.package_family_name.as_deref() {
        return activate_store_app(family).map_err(|err| format!("打开 Codex 失败：{err}"));
    }
    let Some(executable) = install.executable.as_ref() else {
        return Err("没有可打开的 Codex 程序。".to_string());
    };
    let mut command = Command::new(executable);
    if let Some(directory) = executable.parent() {
        command.current_dir(directory);
    }
    command
        .spawn()
        .map(|child| child.id())
        .map_err(|err| format!("打开 Codex 失败：{err}"))
}

#[cfg(windows)]
fn activate_store_app(family: &str) -> windows::core::Result<u32> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{
        CoAllowSetForegroundWindow, CoCreateInstance, CoInitializeEx, CoUninitialize,
        CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Shell::{
        ApplicationActivationManager, IApplicationActivationManager, AO_NOERRORUI,
    };

    let app_id = HSTRING::from(format!("{family}!{STORE_APP_ID}"));
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)?;
        let result = (|| {
            let manager: IApplicationActivationManager =
                CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
            let _ = CoAllowSetForegroundWindow(&manager, None);
            manager.ActivateApplication(&app_id, PCWSTR::null(), AO_NOERRORUI)
        })();
        CoUninitialize();
        result
    }
}

#[cfg(windows)]
fn has_main_window(pid: u32) -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
        GW_OWNER,
    };

    struct WindowSearch {
        pid: u32,
        found: bool,
    }

    unsafe extern "system" fn visit(window: HWND, context: LPARAM) -> BOOL {
        let search = &mut *(context.0 as *mut WindowSearch);
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pid == search.pid
            && IsWindowVisible(window).as_bool()
            && GetWindow(window, GW_OWNER).0 == 0
            && GetWindowTextLengthW(window) > 0
        {
            search.found = true;
            return BOOL(0);
        }
        BOOL(1)
    }

    let mut search = WindowSearch { pid, found: false };
    // EnumWindows invokes the callback synchronously while search is in scope.
    let _ = unsafe {
        EnumWindows(
            Some(visit),
            LPARAM(&mut search as *mut WindowSearch as isize),
        )
    };
    search.found
}

#[cfg(windows)]
fn discover_installed() -> Result<DesktopInstall, String> {
    let mut found = Vec::new();
    if let Some(windowsapps) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
        collect_store_installs(&windowsapps.join("WindowsApps"), &mut found);
    }
    collect_store_installs(Path::new(r"C:\Program Files\WindowsApps"), &mut found);
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local);
        for relative in [
            "Programs\\ChatGPT\\ChatGPT.exe",
            "Programs\\Codex\\Codex.exe",
        ] {
            let exe = root.join(relative);
            if exe.is_file() {
                if let Some(install) = install_from_path(&exe) {
                    found.push(install);
                }
            }
        }
        collect_execution_aliases(&root.join("Microsoft\\WindowsApps"), &mut found);
    }
    found.sort_by(|left, right| install_rank(right).cmp(&install_rank(left)));
    found.dedup_by(|left, right| {
        left.package_family_name == right.package_family_name && left.executable == right.executable
    });
    found
        .into_iter()
        .next()
        .ok_or_else(|| "找不到 Codex 桌面程序，账号未切换。".to_string())
}

#[cfg(windows)]
fn collect_store_installs(windowsapps: &Path, found: &mut Vec<DesktopInstall>) {
    let Ok(entries) = fs::read_dir(windowsapps) else {
        return;
    };
    for entry in entries.flatten() {
        let app_path = entry.path().join("app");
        for executable in ["ChatGPT.exe", "Codex.exe"] {
            let exe = app_path.join(executable);
            if exe.is_file() {
                if let Some(install) = install_from_path(&exe) {
                    found.push(install);
                }
            }
        }
    }
}

#[cfg(windows)]
fn collect_execution_aliases(windowsapps: &Path, found: &mut Vec<DesktopInstall>) {
    let Ok(entries) = fs::read_dir(windowsapps) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("openai.codex") && !lower.contains("chatgpt") {
            found.push(DesktopInstall {
                executable: None,
                package_family_name: Some(name),
            });
        }
    }
}

#[cfg(windows)]
fn install_rank(install: &DesktopInstall) -> (u8, u8, String) {
    let family = install
        .package_family_name
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let stable = u8::from(family.starts_with("openai.codex_"));
    let has_exe = u8::from(install.executable.is_some());
    (stable, has_exe, family)
}

#[cfg(windows)]
fn snapshot_processes() -> Vec<ObservedProcess> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessRefreshKind::new()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| ObservedProcess {
            pid: pid.as_u32(),
            parent_pid: process
                .parent()
                .map(|parent| parent.as_u32())
                .filter(|parent| *parent != 0),
            name: process.name().to_string(),
            exe: process.exe().map(Path::to_path_buf),
            cmd: process.cmd().to_vec(),
        })
        .collect()
}

#[cfg(windows)]
fn run_hidden(program: &str, args: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|_| ())
        .map_err(|err| format!("无法关闭 Codex：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn process(
        pid: u32,
        parent: Option<u32>,
        name: &str,
        exe: Option<&str>,
        cmd: &[&str],
    ) -> ObservedProcess {
        ObservedProcess {
            pid,
            parent_pid: parent,
            name: name.to_string(),
            exe: exe.map(PathBuf::from),
            cmd: cmd.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    fn store_exe() -> String {
        r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1120.5083.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe".to_string()
    }

    #[test]
    fn package_family_comes_from_store_directory_name() {
        assert_eq!(
            package_family_from_dir_name("OpenAI.Codex_26.1120.5083.0_x64__2p2nqsd0c76g0")
                .as_deref(),
            Some("OpenAI.Codex_2p2nqsd0c76g0")
        );
        assert_eq!(
            package_family_from_executable(Path::new(&store_exe())).as_deref(),
            Some("OpenAI.Codex_2p2nqsd0c76g0")
        );
    }

    #[test]
    fn store_codex_is_desktop_and_classic_chatgpt_is_ignored() {
        let store = process(10, None, "ChatGPT.exe", Some(&store_exe()), &[]);
        assert_eq!(classify_process(&store), CodexProcessKind::Desktop);
        let helper = process(
            11,
            Some(10),
            "ChatGPT.exe",
            Some(&store_exe()),
            &["--type=renderer"],
        );
        assert_eq!(classify_process(&helper), CodexProcessKind::Desktop);
        let classic = process(
            12,
            None,
            "ChatGPT.exe",
            Some(
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.0.0.0_x64__abc\app\ChatGPT.exe",
            ),
            &[],
        );
        assert_eq!(classify_process(&classic), CodexProcessKind::Ignore);
        let unknown = process(13, None, "ChatGPT.exe", Some(r"D:\tools\ChatGPT.exe"), &[]);
        assert_eq!(classify_process(&unknown), CodexProcessKind::UnknownChatGpt);
    }

    #[test]
    fn official_app_server_is_stopped_with_its_children_only() {
        let cli = r"C:\Users\Master\AppData\Local\OpenAI\Codex\bin\codex.exe";
        let server = process(20, Some(10), "codex.exe", Some(cli), &["app-server"]);
        assert_eq!(classify_process(&server), CodexProcessKind::AppServer);
        let interactive = process(21, None, "codex.exe", Some(cli), &[]);
        assert_eq!(classify_process(&interactive), CodexProcessKind::Ignore);
        let foreign = process(
            22,
            None,
            "codex.exe",
            Some(r"D:\work\codex.exe"),
            &["app-server"],
        );
        assert_eq!(classify_process(&foreign), CodexProcessKind::Ignore);

        let processes = vec![
            process(10, None, "ChatGPT.exe", Some(&store_exe()), &[]),
            server,
            process(
                30,
                Some(20),
                "python.exe",
                Some(r"C:\Python\python.exe"),
                &[],
            ),
            interactive,
            process(
                40,
                None,
                "CodexManager.exe",
                Some(r"D:\CodexManager\CodexManager.exe"),
                &[],
            ),
        ];
        let stopped = processes_to_stop(&processes).expect("stop list");
        assert!(stopped.contains(&10));
        assert!(stopped.contains(&20));
        assert!(stopped.contains(&30));
        assert!(!stopped.contains(&21));
        assert!(!stopped.contains(&40));
    }

    #[test]
    fn stop_list_keeps_the_switching_service_and_manager_running() {
        let current = std::process::id();
        let processes = vec![
            process(10, None, "ChatGPT.exe", Some(&store_exe()), &[]),
            process(20, Some(10), "CodexManager.exe", None, &[]),
            process(21, Some(20), "msedgewebview2.exe", None, &[]),
            process(current, Some(10), "test-service.exe", None, &[]),
            process(30, Some(current), "test-helper.exe", None, &[]),
            process(40, Some(20), "ChatGPT.exe", Some(&store_exe()), &[]),
        ];

        let mut stopped = processes_to_stop(&processes).expect("stop list");
        stopped.sort_unstable();
        assert_eq!(stopped, vec![10, 40]);
    }

    #[test]
    fn unknown_chatgpt_cancels_before_any_stop_list() {
        let processes = vec![process(
            13,
            None,
            "ChatGPT.exe",
            Some(r"D:\tools\ChatGPT.exe"),
            &[],
        )];
        let error = processes_to_stop(&processes).expect_err("unknown host");
        assert!(error.contains("PID 13"));
    }

    #[test]
    fn running_store_process_selects_its_package() {
        let processes = vec![process(10, None, "ChatGPT.exe", Some(&store_exe()), &[])];
        let install = select_running_install(&processes).expect("install");
        assert_eq!(
            install.package_family_name.as_deref(),
            Some("OpenAI.Codex_2p2nqsd0c76g0")
        );
    }

    #[test]
    fn auth_identity_requires_the_same_account() {
        let expected = identity_from_auth_json(&auth_json("acct-1", "ws-1")).expect("expected");
        let same = identity_from_auth_json(&auth_json("acct-1", "ws-1")).expect("same");
        let other = identity_from_auth_json(&auth_json("acct-1", "ws-2")).expect("other");
        let replaced = identity_from_auth_json(&auth_json("acct-2", "ws-2")).expect("replaced");
        assert!(auth_identity_matches(&expected, &same));
        assert!(!auth_identity_matches(&expected, &other));
        assert!(!auth_identity_matches(&expected, &replaced));
    }

    #[test]
    fn launch_confirmation_requires_a_window_and_stable_matching_auth() {
        let path = std::env::temp_dir().join(format!(
            "codexmanager-launch-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let auth = auth_json("acct-1", "ws-1");
        let expected = identity_from_auth_json(&auth).expect("expected identity");
        fs::write(&path, auth).expect("write matching auth");
        let no_window =
            wait_for_window_and_auth(&path, &expected, Duration::ZERO, Duration::ZERO, || false);
        let not_stable = wait_for_window_and_auth(
            &path,
            &expected,
            Duration::ZERO,
            Duration::from_secs(3),
            || true,
        );
        let ready =
            wait_for_window_and_auth(&path, &expected, Duration::ZERO, Duration::ZERO, || true);
        fs::write(&path, auth_json("acct-2", "ws-2")).expect("write different auth");
        let wrong_account =
            wait_for_window_and_auth(&path, &expected, Duration::ZERO, Duration::ZERO, || true);
        fs::remove_file(path).expect("remove test auth");

        assert!(no_window.expect_err("missing window").contains("未检测到"));
        assert!(not_stable.expect_err("unstable auth").contains("尚未稳定"));
        assert!(ready.is_ok());
        assert!(wrong_account
            .expect_err("different account")
            .contains("不一致"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the developer machine's Codex desktop install"]
    fn developer_machine_codex_processes_are_identified() {
        let processes = snapshot_processes();
        let current = std::process::id();
        let pids = processes_to_stop(&processes).expect("ambiguous ChatGPT.exe");
        assert!(
            !pids.contains(&current),
            "stop list includes the test process"
        );
        let install = select_running_install(&processes).or_else(|| discover_installed().ok());
        assert!(install.is_some(), "Codex desktop install was not found");
    }

    fn auth_json(account_id: &str, chatgpt_account_id: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{chatgpt_account_id}"}}}}"#
        ));
        format!(
            r#"{{"tokens":{{"account_id":"{account_id}","access_token":"{header}.{payload}.sig"}}}}"#
        )
    }
}
