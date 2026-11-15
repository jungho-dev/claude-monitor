//! tui/win_ctrl.rs
//! 터미널 창 최상단 고정(HWND_TOPMOST)과 클래식 콘솔 글꼴 적용·복원 (Win32 직접 선언).

#[cfg(windows)]
use std::sync::OnceLock;

// 1. Win32 FFI 선언 -----------------------------------------------------------
#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    pub type Handle = *mut c_void;

    pub const HWND_TOPMOST: Hwnd = -1isize as Hwnd;
    pub const HWND_NOTOPMOST: Hwnd = -2isize as Hwnd;
    pub const SWP_NOSIZE: u32 = 0x0001;
    pub const SWP_NOMOVE: u32 = 0x0002;
    pub const SWP_SHOWWINDOW: u32 = 0x0040;
    pub const GA_ROOT: u32 = 2;
    pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    // FF_MODERN(고정폭 계열) | TMPF_TRUETYPE, 굵기 FW_NORMAL
    pub const FONT_FAMILY_TRUETYPE_MODERN: u32 = 0x30 | 0x04;
    pub const FW_NORMAL: u32 = 400;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Coord {
        pub x: i16,
        pub y: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ConsoleFontInfoEx {
        pub cb_size: u32,
        pub n_font: u32,
        pub dw_font_size: Coord,
        pub font_family: u32,
        pub font_weight: u32,
        pub face_name: [u16; 32],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetConsoleWindow() -> Hwnd;
        pub fn GetStdHandle(std_handle: u32) -> Handle;
        pub fn GetCurrentConsoleFontEx(
            output: Handle,
            maximum_window: i32,
            font: *mut ConsoleFontInfoEx,
        ) -> i32;
        pub fn SetCurrentConsoleFontEx(
            output: Handle,
            maximum_window: i32,
            font: *const ConsoleFontInfoEx,
        ) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn GetForegroundWindow() -> Hwnd;
        pub fn GetAncestor(hwnd: Hwnd, ga_flags: u32) -> Hwnd;
        pub fn GetClassNameW(hwnd: Hwnd, class_name: *mut u16, max_count: i32) -> i32;
        pub fn SetWindowPos(
            hwnd: Hwnd,
            hwnd_insert_after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }
}

#[cfg(windows)]
static INITIAL_TERMINAL_HWND: OnceLock<usize> = OnceLock::new();

// 2. 터미널 창 제어기 ---------------------------------------------------------
// TUI 시작 직후 포그라운드 창(= 이 터미널)을 보관, 이후 최상단 고정 대상
pub fn capture_initial_terminal_window() {
    #[cfg(windows)]
    {
        let _ = INITIAL_TERMINAL_HWND.get_or_init(|| {
            // SAFETY: 인자 없는 조회 호출, 반환 핸들은 null 확인 후에만 사용
            unsafe {
                let foreground = ffi::GetForegroundWindow();
                if !foreground.is_null() {
                    let root = ffi::GetAncestor(foreground, ffi::GA_ROOT);
                    if !root.is_null() {
                        return root as usize;
                    }
                    return foreground as usize;
                }
                ffi::GetConsoleWindow() as usize
            }
        });
    }
}

// 창 최상단 고정 켜기·끄기 (창 이동·크기는 그대로)
pub fn set_terminal_topmost(enabled: bool) {
    #[cfg(windows)]
    {
        let mode = if enabled {
            ffi::HWND_TOPMOST
        } else {
            ffi::HWND_NOTOPMOST
        };
        let initial_hwnd = INITIAL_TERMINAL_HWND.get().copied().unwrap_or(0) as ffi::Hwnd;
        // SAFETY: 창 핸들은 null 확인 후 전달, 위치·크기 변경 없는 플래그
        unsafe {
            let console_hwnd = ffi::GetConsoleWindow();
            for hwnd in [initial_hwnd, console_hwnd] {
                if hwnd.is_null() {
                    continue;
                }
                let _ = ffi::SetWindowPos(
                    hwnd,
                    mode,
                    0,
                    0,
                    0,
                    0,
                    ffi::SWP_NOMOVE | ffi::SWP_NOSIZE | ffi::SWP_SHOWWINDOW,
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
    }
}

// 3. 클래식 콘솔 글꼴 적용·복원 -----------------------------------------------
// conhost 창에서만 적용, Windows Terminal·VS Code 등 ConPTY 호스트는 각자 프로필 글꼴을 씀 (앱이 못 바꿈)
// 가드가 사라질 때(TUI 종료) 실행 전 글꼴로 되돌림
pub struct ConsoleFontGuard {
    #[cfg(windows)]
    previous: ffi::ConsoleFontInfoEx,
}

impl Drop for ConsoleFontGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: previous 는 GetCurrentConsoleFontEx 가 채운 cb_size 초기화 구조체
        unsafe {
            let output = ffi::GetStdHandle(ffi::STD_OUTPUT_HANDLE);
            if !output.is_null() && output != ffi::INVALID_HANDLE_VALUE {
                let _ = ffi::SetCurrentConsoleFontEx(output, 0, &self.previous);
            }
        }
    }
}

// 적용 성공 시 복원 가드, 적용하지 않은 이유는 Err 문구로 반환 (실행은 계속)
pub fn apply_console_font(face: &str) -> Result<ConsoleFontGuard, String> {
    if face.is_empty() {
        return Err("글꼴 변경 끔 (빈 글꼴 이름)".to_string());
    }
    #[cfg(windows)]
    {
        let face_units: Vec<u16> = face.encode_utf16().collect();
        if face_units.len() >= 32 {
            return Err(format!("글꼴 이름이 31자 초과: {face}"));
        }

        // SAFETY: 구조체는 cb_size 를 채운 POD, 출력 핸들·창 핸들은 호출 직전 확인
        unsafe {
            // 1. 실제 conhost 창만 대상 (ConPTY 의사 창은 PseudoConsoleWindow)
            // WT_SESSION 환경 변수는 WT 에서 띄운 conhost 창에도 상속되므로 창 클래스로 판별
            let console_hwnd = ffi::GetConsoleWindow();
            let mut class_buf = [0u16; 64];
            let class_len = if console_hwnd.is_null() {
                0
            } else {
                ffi::GetClassNameW(console_hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32)
            };
            let class_name = String::from_utf16_lossy(&class_buf[..class_len.max(0) as usize]);
            if class_name != "ConsoleWindowClass" {
                return Err(format!(
                    "Windows Terminal 등 ConPTY 터미널({class_name}): 터미널 프로필 글꼴 사용"
                ));
            }

            let output = ffi::GetStdHandle(ffi::STD_OUTPUT_HANDLE);
            if output.is_null() || output == ffi::INVALID_HANDLE_VALUE {
                return Err("콘솔 출력 핸들 없음".to_string());
            }

            // 2. 현재 글꼴 보관
            let mut previous: ffi::ConsoleFontInfoEx = std::mem::zeroed();
            previous.cb_size = std::mem::size_of::<ffi::ConsoleFontInfoEx>() as u32;
            if ffi::GetCurrentConsoleFontEx(output, 0, &mut previous) == 0 {
                return Err("현재 콘솔 글꼴 조회 실패 (콘솔 창 아님)".to_string());
            }

            // 3. 같은 높이로 TrueType 글꼴 지정 (래스터 글꼴 높이 0 이면 16)
            let mut wanted = previous;
            wanted.n_font = 0;
            wanted.dw_font_size = ffi::Coord {
                x: 0,
                y: if previous.dw_font_size.y > 0 {
                    previous.dw_font_size.y
                } else {
                    16
                },
            };
            wanted.font_family = ffi::FONT_FAMILY_TRUETYPE_MODERN;
            wanted.font_weight = ffi::FW_NORMAL;
            wanted.face_name = [0; 32];
            wanted.face_name[..face_units.len()].copy_from_slice(&face_units);
            if ffi::SetCurrentConsoleFontEx(output, 0, &wanted) == 0 {
                return Err(format!("콘솔 글꼴 적용 실패: {face}"));
            }

            // 4. 미설치 글꼴은 콘솔이 다른 글꼴로 대체하므로 재조회로 확인, 불일치면 원복
            let mut applied: ffi::ConsoleFontInfoEx = std::mem::zeroed();
            applied.cb_size = previous.cb_size;
            let _ = ffi::GetCurrentConsoleFontEx(output, 0, &mut applied);
            let applied_len = applied
                .face_name
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(32);
            let applied_face = String::from_utf16_lossy(&applied.face_name[..applied_len]);
            if !applied_face.eq_ignore_ascii_case(face) {
                let _ = ffi::SetCurrentConsoleFontEx(output, 0, &previous);
                return Err(format!(
                    "글꼴 미설치 또는 콘솔 거부: {face} (현재 {applied_face})"
                ));
            }
            Ok(ConsoleFontGuard { previous })
        }
    }

    #[cfg(not(windows))]
    {
        Err("Windows 전용 기능".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_face_name_is_rejected_without_touching_the_console() {
        assert!(apply_console_font("").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn overlong_face_name_is_rejected_before_any_win32_call() {
        let too_long = "가".repeat(32);
        let reason = apply_console_font(&too_long).err().unwrap();
        assert!(reason.contains("31자 초과"));
    }
}
