//! build.rs
//! Windows 실행 파일에 앱 아이콘·버전 정보·asInvoker manifest 리소스를 windres 로 컴파일해 링크.
//! gnullvm 타겟은 libunwind.dll 을 동적 링크하므로 target/<profile> 옆에 복사해 PATH 없이 실행되게 함.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const DLL_NAME: &str = "libunwind.dll";
const APP_NAME: &str = "rust-agents-proxy";

fn main() {
    let icon_dir = PathBuf::from("assets").join("icon");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!(
        "cargo:rerun-if-changed={}",
        icon_dir.join(format!("{APP_NAME}.rc")).display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        icon_dir.join(format!("{APP_NAME}.ico")).display()
    );
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RUSTC_LINKER");

    let target = env::var("TARGET").unwrap_or_default();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources(&icon_dir);
    }
    if target.contains("gnullvm") {
        copy_libunwind_dll(&target);
    }
}

// 1. 리소스 컴파일 및 링크 ----------------------------------------------------
// 메타데이터·manifest 없는 서명 없는 exe 는 백신 ML 휴리스틱 감점 요인이라 링크 시점에 삽입.
// 리소스 도구가 없어도 빌드는 계속 (리소스만 빠짐).
fn embed_windows_resources(icon_dir: &Path) {
    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        println!("cargo:warning=OUT_DIR 없음, 리소스 삽입 생략");
        return;
    };
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let [major, minor, patch] = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap_or_else(|_| "0".into()));

    // 1. manifest 작성 (rc 가 --include-dir 로 참조)
    let manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="{APP_NAME}" version="{major}.{minor}.{patch}.0" processorArchitecture="*"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
</assembly>
"#
    );
    if let Err(err) = std::fs::write(out_dir.join(format!("{APP_NAME}.manifest")), manifest) {
        println!("cargo:warning=manifest 쓰기 실패 ({err}), 리소스 삽입 생략");
        return;
    }

    // 2. windres 후보: WINDRES 지정값, 링커 폴더의 mingw/llvm windres, PATH 순
    let tool_names = ["x86_64-w64-mingw32-windres", "llvm-windres", "windres"];
    let mut candidates: Vec<PathBuf> = env::var_os("WINDRES")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(linker_dir) = env::var_os("RUSTC_LINKER")
        .map(PathBuf::from)
        .and_then(|linker| linker.parent().map(Path::to_path_buf))
    {
        candidates.extend(
            tool_names
                .iter()
                .map(|tool| linker_dir.join(format!("{tool}.exe")))
                .filter(|exe_path| exe_path.is_file()),
        );
    }
    candidates.extend(tool_names.iter().map(PathBuf::from));

    // 3. 첫 성공 후보의 오브젝트를 링크
    let rc_path = icon_dir.join(format!("{APP_NAME}.rc"));
    let res_obj = out_dir.join(format!("{APP_NAME}-res.o"));
    for windres in &candidates {
        let status = Command::new(windres)
            .arg("--input")
            .arg(&rc_path)
            .arg("--output")
            .arg(&res_obj)
            .args(["--output-format", "coff", "--include-dir"])
            .arg(icon_dir)
            .arg("--include-dir")
            .arg(&out_dir)
            .arg(format!("-DRAP_VER_MAJOR={major}"))
            .arg(format!("-DRAP_VER_MINOR={minor}"))
            .arg(format!("-DRAP_VER_PATCH={patch}"))
            // windres 는 GNU 방식으로 인자 따옴표를 한 번 벗기므로 역슬래시로 보존
            .arg(format!("-DRAP_VER_TEXT=\\\"{version}\\\""))
            .status();
        if matches!(status, Ok(code) if code.success()) && res_obj.is_file() {
            println!("cargo:rustc-link-arg-bins={}", res_obj.display());
            return;
        }
    }
    println!("cargo:warning=windres 실행 실패, 아이콘·버전 정보·manifest 없이 빌드");
}

// 2. libunwind.dll 탐색 -------------------------------------------------------
fn find_libunwind_dll(target: &str) -> Option<PathBuf> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());

    // host 툴체인 bin (host == target 인 일반적인 경우)
    let host_bin = sysroot.join("bin").join(DLL_NAME);
    if host_bin.is_file() {
        return Some(host_bin);
    }

    // 크로스 컴파일 시 타겟 전용 rustlib bin
    let target_bin = sysroot
        .join("lib")
        .join("rustlib")
        .join(target)
        .join("bin")
        .join(DLL_NAME);
    target_bin.is_file().then_some(target_bin)
}

// 3. libunwind.dll 복사 -------------------------------------------------------
// OUT_DIR = <target_dir>/<profile>/build/<pkg>-<hash>/out 이므로 3단계 상위가 exe 폴더.
fn copy_libunwind_dll(target: &str) {
    let Some(dll_src) = find_libunwind_dll(target) else {
        println!(
            "cargo:warning={DLL_NAME} 를 찾지 못함 (target={target}), 실행 시 로드 실패할 수 있음"
        );
        return;
    };
    let Some(profile_dir) = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .and_then(|out_dir| out_dir.ancestors().nth(3).map(Path::to_path_buf))
    else {
        println!("cargo:warning=OUT_DIR 에서 target 디렉터리 경로를 계산하지 못함");
        return;
    };

    // 실행 중인 바이너리가 DLL 을 잠그므로(os error 32) 내용이 같으면 복사하지 않음
    let dest = profile_dir.join(DLL_NAME);
    let src_bytes = std::fs::read(&dll_src).ok();
    if src_bytes.is_some() && src_bytes == std::fs::read(&dest).ok() {
        return;
    }
    if let Err(err) = std::fs::copy(&dll_src, &dest) {
        println!(
            "cargo:warning={DLL_NAME} 복사 실패 ({err}): {}",
            dest.display()
        );
    }
}
