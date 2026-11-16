//! tui/state_writer.rs
//! 상태 파일 저장 스레드: 확정 이벤트의 저장 요청을 채널로 받아 최신 하나로 합쳐 쓰고, 종료 때 남은 요청을 쓴 뒤 합류하며, 실패는 UiMessage 로 돌려줌.

use crate::tui::layout_store::write_layout_atomic;
use crate::tui::state::UiMessage;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SendError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// 종료 때 저장 스레드를 기다리는 최대 시간 (막힌 디스크·동기화 폴더가 종료를 붙잡지 않게)
const JOIN_TIMEOUT: Duration = Duration::from_secs(5);

// 저장 요청 한 건 (path = 상태 파일 경로, text = UI 스레드가 만든 파일 내용 전체)
pub struct StateSave {
    pub path: PathBuf,
    pub text: String,
}

// 1. 저장 스레드 손잡이 -------------------------------------------------------
// 가드가 소유해 어느 종료 경로든 Drop 에서 남은 요청을 쓰고 스레드를 합류시킴
pub struct StateWriter {
    requests: Option<Sender<StateSave>>,
    worker: Option<JoinHandle<()>>,
    // 저장 스레드가 끝나며 보내는 마지막 쓰기 결과 (시간 제한 합류용)
    finished: Receiver<Option<String>>,
}

impl StateWriter {
    // 저장 요청 보내기 (저장 스레드가 이미 끝났으면 요청을 그대로 돌려줌)
    pub fn queue_state_save(&self, save: StateSave) -> Result<(), SendError<StateSave>> {
        match &self.requests {
            Some(requests) => requests.send(save),
            None => Err(SendError(save)),
        }
    }

    // 요청 채널을 닫아 이미 보낸 요청을 모두 쓴 뒤 스레드 합류, 반환 = 마지막 쓰기가 실패로 끝났을 때의 오류 원문
    // 여러 번 불려도 안전 (두 번째부터는 None), JOIN_TIMEOUT 안에 끝나지 않으면 기다리지 않고 그 사실을 오류 원문으로 돌려줌
    pub fn finish_state_saves(&mut self) -> Option<String> {
        self.finish_state_saves_within(JOIN_TIMEOUT)
    }

    fn finish_state_saves_within(&mut self, timeout: Duration) -> Option<String> {
        self.requests = None;
        let worker = self.worker.take()?;
        match self.finished.recv_timeout(timeout) {
            Ok(unresolved) => {
                let _ = worker.join();
                unresolved
            }
            Err(RecvTimeoutError::Timeout) => Some(format!(
                "state writer did not finish within {}s",
                timeout.as_secs()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                None
            }
        }
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        let _ = self.finish_state_saves();
    }
}

// 2. 저장 스레드 시작 ---------------------------------------------------------
// 쓰기 실패는 UiMessage::StateSaveFailed 로 UI 스레드에 알림 (알림 문구는 UI 스레드가 화면 언어로 만듦)
pub fn spawn_state_writer(ui_tx: Sender<UiMessage>) -> io::Result<StateWriter> {
    spawn_state_writer_with(ui_tx, write_layout_atomic)
}

// 쓰기 함수를 받아 시작 (테스트가 막힌 쓰기를 흉내 낼 수 있음)
fn spawn_state_writer_with(
    ui_tx: Sender<UiMessage>,
    write: impl Fn(&Path, &str) -> io::Result<()> + Send + 'static,
) -> io::Result<StateWriter> {
    let (requests, inbox) = mpsc::channel::<StateSave>();
    let (done_tx, finished) = mpsc::channel::<Option<String>>();
    let worker = thread::Builder::new()
        .name("tui-state-writer".to_string())
        .spawn(move || {
            let unresolved = run_state_writer(&inbox, write, |detail| {
                let _ = ui_tx.send(UiMessage::StateSaveFailed { detail });
            });
            let _ = done_tx.send(unresolved);
        })?;
    Ok(StateWriter {
        requests: Some(requests),
        worker: Some(worker),
        finished,
    })
}

// 3. 저장 반복 ----------------------------------------------------------------
// 요청을 받으면 이미 쌓인 요청까지 모아 같은 경로는 가장 나중 것만 쓰고(처음 나온 순서 유지), 실패는 보고하고 계속,
// 보낸 쪽이 모두 닫히면 남은 요청을 쓰고 끝, 반환 = 마지막 쓰기가 실패로 끝났을 때의 오류 원문
// write·report 를 받아 스레드 없이 합치기·실패 보고를 검증할 수 있음
fn run_state_writer(
    inbox: &Receiver<StateSave>,
    write: impl Fn(&Path, &str) -> io::Result<()>,
    report: impl Fn(String),
) -> Option<String> {
    let mut unresolved: Option<String> = None;
    while let Ok(first) = inbox.recv() {
        // 1. 쌓인 요청 모으기
        let mut pending: Vec<StateSave> = vec![first];
        while let Ok(next) = inbox.try_recv() {
            match pending.iter_mut().find(|queued| queued.path == next.path) {
                Some(queued) => queued.text = next.text,
                None => pending.push(next),
            }
        }

        // 2. 쓰기 (성공하면 앞선 실패는 해소)
        for save in &pending {
            match write(&save.path, &save.text) {
                Ok(()) => unresolved = None,
                Err(error) => {
                    let detail = error.to_string();
                    report(detail.clone());
                    unresolved = Some(detail);
                }
            }
        }
    }
    unresolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static DIR_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    // 테스트마다 따로 쓰는 임시 폴더 (끝나면 지움)
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = DIR_SEQUENCE.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("cm-state-writer-{}-{sequence}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn save(path: &Path, text: &str) -> StateSave {
        StateSave {
            path: path.to_path_buf(),
            text: text.to_string(),
        }
    }

    #[test]
    fn a_burst_of_requests_for_one_path_writes_only_the_latest_in_first_seen_order() {
        let (requests, inbox) = mpsc::channel::<StateSave>();
        let first = PathBuf::from("first.toml");
        let second = PathBuf::from("second.toml");
        for (path, text) in [
            (&first, "a"),
            (&first, "b"),
            (&second, "x"),
            (&first, "c"),
            (&second, "y"),
        ] {
            requests.send(save(path, text)).unwrap();
        }
        drop(requests);

        let writes: RefCell<Vec<(PathBuf, String)>> = RefCell::new(Vec::new());
        let reports: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let unresolved = run_state_writer(
            &inbox,
            |path, text| {
                writes
                    .borrow_mut()
                    .push((path.to_path_buf(), text.to_string()));
                Ok(())
            },
            |detail| reports.borrow_mut().push(detail),
        );
        assert_eq!(
            *writes.borrow(),
            [(first, "c".to_string()), (second, "y".to_string())]
        );
        assert!(reports.borrow().is_empty());
        assert_eq!(unresolved, None);
    }

    #[test]
    fn a_failed_write_is_reported_and_later_requests_still_run_and_a_later_success_clears_it() {
        let (requests, inbox) = mpsc::channel::<StateSave>();
        let path = PathBuf::from("state.toml");
        requests.send(save(&path, "one")).unwrap();
        drop(requests);
        let reports: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let unresolved = run_state_writer(
            &inbox,
            |_, _| Err(io::Error::new(ErrorKind::PermissionDenied, "access denied")),
            |detail| reports.borrow_mut().push(detail),
        );
        assert_eq!(*reports.borrow(), ["access denied"]);
        assert_eq!(unresolved.as_deref(), Some("access denied"));

        // 실패 뒤 같은 반복에서 성공하면 남은 실패 없음 (보고는 이미 나감)
        let (requests, inbox) = mpsc::channel::<StateSave>();
        let calls = RefCell::new(0usize);
        let (first_path, second_path) = (PathBuf::from("a"), PathBuf::from("b"));
        requests.send(save(&first_path, "1")).unwrap();
        requests.send(save(&second_path, "2")).unwrap();
        drop(requests);
        let unresolved = run_state_writer(
            &inbox,
            |_, _| {
                *calls.borrow_mut() += 1;
                if *calls.borrow() == 1 {
                    Err(io::Error::other("disk full"))
                } else {
                    Ok(())
                }
            },
            |_| {},
        );
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(unresolved, None);
    }

    #[test]
    fn dropping_the_writer_flushes_queued_requests_before_it_returns() {
        let dir = TempDir::new();
        let path = dir.0.join("nested").join("tui.toml");
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let writer = spawn_state_writer(ui_tx).unwrap();
        writer
            .queue_state_save(save(&path, "version = 1\n"))
            .unwrap();
        writer
            .queue_state_save(save(&path, "version = 1\nlanguage = \"ko\"\n"))
            .unwrap();
        drop(writer);

        // 합류가 끝난 뒤라 파일은 마지막 요청 내용이고 임시 파일도 남지 않음
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "version = 1\nlanguage = \"ko\"\n"
        );
        let names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["tui.toml".to_string()]);
        assert!(ui_rx.try_recv().is_err());
    }

    #[test]
    fn a_failed_save_comes_back_as_a_typed_message_and_the_final_flush_reports_it() {
        let dir = TempDir::new();
        // 대상 경로가 폴더라 이름 바꾸기가 실패함
        let path = dir.0.join("tui.toml");
        fs::create_dir_all(&path).unwrap();
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let mut writer = spawn_state_writer(ui_tx).unwrap();
        writer
            .queue_state_save(save(&path, "version = 1\n"))
            .unwrap();

        let unresolved = writer.finish_state_saves();
        let detail = match ui_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            UiMessage::StateSaveFailed { detail } => detail,
            other => panic!("예상 밖 메시지: {other:?}"),
        };
        assert!(!detail.is_empty());
        assert_eq!(unresolved, Some(detail));
        let leftovers: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // 끝난 뒤에는 요청을 그대로 돌려주고, 다시 끝내도 안전
        let lost = writer.queue_state_save(save(&path, "late")).unwrap_err();
        assert_eq!(lost.0.text, "late");
        assert_eq!(writer.finish_state_saves(), None);
    }

    #[test]
    fn a_stuck_write_does_not_hang_the_final_join_and_is_reported_as_unresolved() {
        let (ui_tx, _ui_rx) = mpsc::channel::<UiMessage>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let mut writer = spawn_state_writer_with(ui_tx, move |_, _| {
            let _ = release_rx.recv();
            Ok(())
        })
        .unwrap();
        writer
            .queue_state_save(save(Path::new("stuck.toml"), "x"))
            .unwrap();

        // 쓰기가 끝나지 않아도 제한 시간 뒤에 돌아와 사유를 알림 (합류를 무한정 기다리지 않음)
        let started = std::time::Instant::now();
        let detail = writer.finish_state_saves_within(Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(
            detail.as_deref(),
            Some("state writer did not finish within 0s")
        );

        // 다시 끝내도 안전, 풀어 준 스레드는 알아서 끝남
        assert_eq!(writer.finish_state_saves(), None);
        let _ = release_tx.send(());
    }

    #[test]
    fn the_final_join_timeout_is_five_seconds() {
        assert_eq!(JOIN_TIMEOUT, Duration::from_secs(5));
    }
}
