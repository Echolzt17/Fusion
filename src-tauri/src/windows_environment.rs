//! Non-identifying environment metadata only. No user names, paths or registry dumps.
use serde::Serialize;
use windows::Win32::System::SystemInformation::{GetNativeSystemInfo, OSVERSIONINFOW, SYSTEM_INFO};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    os: &'static str,
    os_build: Option<u32>,
    architecture: &'static str,
    webview2_version: Option<String>,
    window_scale: Option<f64>,
}

fn windows_name(major: u32, build: u32) -> &'static str {
    match (major, build) {
        (10, 22000..) => "Windows 11",
        (10, _) => "Windows 10",
        _ => "Windows (unknown)",
    }
}

pub fn collect(scale: Option<f64>) -> Environment {
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(version: *mut OSVERSIONINFOW) -> i32;
    }
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let version_ok = unsafe { RtlGetVersion(&mut version) } >= 0;
    let mut system = SYSTEM_INFO::default();
    unsafe {
        GetNativeSystemInfo(&mut system);
    }
    let architecture = match unsafe { system.Anonymous.Anonymous.wProcessorArchitecture.0 } {
        9 => "x64",
        0 => "x86",
        12 => "arm64",
        _ => "unknown",
    };
    Environment {
        os: if version_ok {
            windows_name(version.dwMajorVersion, version.dwBuildNumber)
        } else {
            "Windows (unknown)"
        },
        os_build: version_ok.then_some(version.dwBuildNumber),
        architecture,
        webview2_version: tauri::webview_version().ok(),
        window_scale: scale,
    }
}

pub fn report_environment(scale: Option<f64>, desktop: Option<&str>, engine: Option<&str>, uptime: u64) -> fusion_report_protocol::Environment {
    let e=collect(scale);
    fusion_report_protocol::Environment {app_version:env!("CARGO_PKG_VERSION").into(),os:e.os.into(),os_build:e.os_build,architecture:e.architecture.into(),
        webview2_version:e.webview2_version.as_deref().and_then(fusion_report_protocol::version),desktop_version:desktop.and_then(fusion_report_protocol::version),engine_version:engine.and_then(fusion_report_protocol::version),uptime_seconds:uptime}
}

// Used by the installer's isolated helper before any database, Vault or Tauri initialization.
pub fn webview_runtime_ready() -> bool {
    let config: serde_json::Value = match serde_json::from_str(include_str!("../tauri.conf.json")) {
        Ok(config) => config,
        Err(_) => return false,
    };
    let Some(minimum) = config["bundle"]["windows"]["minimumWebview2Version"].as_str() else {
        return false;
    };
    tauri::webview_version().is_ok_and(|actual| version_at_least(&actual, minimum))
}
fn version_at_least(actual: &str, minimum: &str) -> bool {
    let parse = |s: &str| -> Option<[u32; 4]> {
        let parts: Vec<u32> = s
            .split('.')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        parts.try_into().ok()
    };
    matches!((parse(actual), parse(minimum)), (Some(a), Some(m)) if a >= m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_windows_11_by_build_without_requiring_major_11() {
        assert_eq!(windows_name(10, 19045), "Windows 10");
        for build in [22000, 22631, 26100, 26200, 26300, 28000] {
            assert_eq!(windows_name(10, build), "Windows 11");
        }
        assert_eq!(windows_name(6, 7601), "Windows (unknown)");
        // A bundled patch update must not force a reinstall of a compatible Evergreen runtime.
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let minimum = config["bundle"]["windows"]["minimumWebview2Version"].as_str().unwrap();
        assert!(version_at_least("156.0.4314.50", minimum));
        assert!(version_at_least("155.0.4283.62", minimum));
        assert!(!version_at_least("109.0.1518.78", minimum));
        assert!(version_at_least("156.0.4314.56", "156.0.4314.56"));
        assert!(version_at_least("157.0.1.1", "156.0.4314.56"));
        for actual in [
            "",
            "0.0.0.0",
            "109.0.0.0",
            "invalid",
            "156.0.4314.55",
            "156.0.4314.56 beta",
        ] {
            assert!(!version_at_least(actual, "156.0.4314.56"));
        }
        let value = serde_json::to_value(collect(Some(1.75))).unwrap();
        assert_eq!(value["architecture"], "x64");
        assert!(value["osBuild"].as_u64().is_some());
        assert_eq!(value["windowScale"], 1.75);
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
}
