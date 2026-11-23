//! [도움말 > mome 업데이트] 대화상자와 그 상태: 확인, 내려받아 설치, 새 버전으로 다시 시작.
//!
//! 인터넷에 나가는 일과 파일을 바꾸는 일은 따로 뗀 스레드에서 돌고([`crate::update`]), 결과는 채널로 받는다.
//! 상태는 대화상자가 아니라 앱에 둔다: 대화상자를 닫아도 설치는 계속되고, 다시 열면 이어서 보여 준다.

use std::sync::mpsc::{channel, Receiver, Sender};

use egui::{Align, Context, Layout, ProgressBar, RichText, ScrollArea, ViewportCommand};

use super::{answer, hint, modal, primary_button, secondary, secondary_button, Dialog, Outcome, BUTTON_SIZE};
use crate::app::MomeApp;
use crate::update::{self, Stage, UpdateInfo};

/// 창의 폭이 이보다 좁으면 단추를 두 줄로 나눠 놓는다(포인트).
const NARROW_WINDOW: f32 = 600.0;

/// 릴리즈 설명을 이만큼만 보여 준다(글자 수).
const NOTES_LIMIT: usize = 4000;

/// 따로 뗀 스레드에서 돌고 있는 일.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Busy {
    Checking,
    Installing,
    Restarting,
}

/// 스레드가 알려 오는 것.
enum Event {
    Checked(Result<UpdateInfo, String>),
    Stage(Stage),
    Installed(Result<String, String>),
    Restarted(Result<(), String>),
}

/// 대화상자의 강조된 단추가 하는 일.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Check,
    Install,
    Restart,
}

pub struct Updater {
    busy: Option<Busy>,
    /// 마지막으로 확인한 결과.
    info: Option<UpdateInfo>,
    /// 마지막 확인·설치·다시 시작이 실패한 까닭.
    error: Option<String>,
    /// 설치가 마지막으로 알려 온 단계.
    stage: Option<Stage>,
    /// 이번 실행에서 설치를 마친 버전(다시 시작해야 그 버전으로 뜬다).
    installed: Option<String>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for Updater {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            busy: None,
            info: None,
            error: None,
            stage: None,
            installed: None,
            tx,
            rx,
        }
    }
}

impl Updater {
    /// `work`를 따로 뗀 스레드에서 돌린다. `work`는 받은 함수로 결과를 알리고, 그때마다 화면을 다시 그리게 한다.
    fn spawn(&mut self, ctx: &Context, busy: Busy, work: impl FnOnce(&dyn Fn(Event)) + Send + 'static) {
        self.busy = Some(busy);
        self.error = None;
        self.stage = None;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            work(&|event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            });
        });
    }

    /// 새 버전의 창이 문서를 넘겨받고 있는 중인지(그동안 이 창은 닫지 않는다).
    pub(in crate::app) fn is_restarting(&self) -> bool {
        self.busy == Some(Busy::Restarting)
    }

    /// 확인한 최신 버전이 실행 파일 폴더에 이미 놓여 있는지(다시 시작만 하면 된다).
    fn ready_to_restart(&self) -> bool {
        self.info.as_ref().is_some_and(|info| info.on_disk || self.installed.as_deref() == Some(info.latest.as_str()))
    }

    fn action(&self) -> Action {
        match &self.info {
            Some(_) if self.ready_to_restart() => Action::Restart,
            Some(info) if info.newer && info.blocked.is_none() => Action::Install,
            _ => Action::Check,
        }
    }

    /// "최신 버전" 칸에 적을 글.
    fn latest_text(&self) -> String {
        match (&self.info, self.busy) {
            (_, Some(Busy::Checking)) => "확인하는 중…".to_owned(),
            (Some(info), _) if info.published.is_empty() => info.latest.clone(),
            (Some(info), _) => format!("{} ({})", info.latest, info.published),
            (None, _) => "아직 확인하지 않았습니다".to_owned(),
        }
    }

    /// 지금 상태를 한 줄로.
    fn verdict(&self) -> Option<String> {
        if let Some(busy) = self.busy {
            return match busy {
                Busy::Checking => None,
                Busy::Installing => Some(stage_text(self.stage)),
                Busy::Restarting => Some("새 버전을 띄우는 중… 새 창이 뜨면 이 창은 닫힙니다.".to_owned()),
            };
        }
        let info = self.info.as_ref()?;
        Some(if self.ready_to_restart() {
            let how = if self.installed.is_some() { "설치를 마쳤습니다" } else { "다른 창에서 이미 설치되어 있습니다" };
            format!("새 버전({}) {how}. 다시 시작하면 새 버전으로 열립니다.", info.latest)
        } else if !info.newer {
            "최신 버전을 쓰고 있습니다.".to_owned()
        } else if let Some(reason) = &info.blocked {
            format!("새 버전이 있습니다. {reason}")
        } else {
            "새 버전이 있습니다.".to_owned()
        })
    }
}

/// 오른쪽으로 붙인 단추 한 줄(먼저 넣은 단추가 가장 오른쪽). 줄의 높이를 정해 둔다: 높이를 정하지 않은 줄은
/// 남은 높이의 가운데에 단추를 놓으므로, 그 아래에 줄이 더 있으면 대화상자가 프레임마다 커진다.
fn button_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), BUTTON_SIZE.y);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), add).inner
}

fn megabytes(bytes: u64) -> String {

    format!("{:.1}MB", bytes as f64 / (1u64 << 20) as f64)
}

fn stage_text(stage: Option<Stage>) -> String {
    match stage {
        None | Some(Stage::Check) => "최신 릴리즈를 확인하는 중…".to_owned(),
        Some(Stage::Download(done, total)) if total > 0 => {
            format!("내려받는 중… {} / {} ({}%)", megabytes(done), megabytes(total), done.saturating_mul(100) / total)
        }
        Some(Stage::Download(..)) => "내려받는 중…".to_owned(),
        Some(Stage::Unpack) => "내려받은 묶음을 확인하고 푸는 중…".to_owned(),
        Some(Stage::Replace) => "실행 파일을 바꾸는 중…".to_owned(),
    }
}

/// 릴리즈 설명이 길면 앞부분만.
fn clipped_notes(notes: &str) -> String {
    match notes.char_indices().nth(NOTES_LIMIT) {
        Some((at, _)) => format!("{}\n…", &notes[..at]),
        None => notes.to_owned(),
    }
}

impl MomeApp {
    pub(super) fn update_ui(&self, ctx: &Context) -> Outcome {
        let p = self.palette();
        let up = &self.updater;
        let action = up.action();
        // 창이 낮으면 릴리즈 설명이 줄어들고, 그래도 넘치면 내용 전체가 스크롤된다(제목과 단추는 남는다).
        let window = ctx.content_rect();
        let notes_height = (window.height() - 440.0).clamp(60.0, 220.0);
        // 단추가 넷까지 늘어난다. 창이 좁으면 두 줄로 나눈다(한 줄로만 놓으면 왼쪽 단추가 잘려 누를 수 없다).
        let two_rows = window.width() < NARROW_WINDOW;
        let body_height = (window.height() - if two_rows { 246.0 } else { 210.0 }).max(48.0);
        let (choice, dismissed) = modal(ctx, "dlg-update", 500.0, "mome 업데이트", |ui| {
            let body = ScrollArea::vertical().id_salt("update-body").max_height(body_height).auto_shrink([false, true]);
            body.show(ui, |ui| {
                egui::Grid::new("update-grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    ui.label("지금 버전");
                    ui.label(RichText::new(update::running_version()).strong());
                    ui.end_row();
                    ui.label("최신 버전");
                    ui.label(RichText::new(up.latest_text()).strong());
                    ui.end_row();
                });
                ui.add_space(8.0);

                if let Some(text) = up.verdict() {
                    match (up.busy, up.stage) {
                        (Some(Busy::Installing), Some(Stage::Download(done, total))) if total > 0 => {
                            ui.label(text);
                            ui.add(ProgressBar::new(done as f32 / total as f32));
                        }
                        (Some(_), _) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(text);
                            });
                        }
                        (None, _) => {
                            ui.label(text);
                        }
                    }
                }
                if up.busy == Some(Busy::Installing) {
                    ui.add_space(4.0);
                    hint(
                        ui,
                        "회선에 따라 몇 초에서 몇 분 걸립니다. 그동안 이 창을 닫고 계속 편집해도 설치는 이어집니다.",
                        &p,
                    );
                }
                if let Some(error) = &up.error {
                    ui.label(RichText::new(error).color(p.warning));
                }

                let notes = up.info.as_ref().filter(|info| info.newer && !info.notes.is_empty());
                if let Some(info) = notes {
                    ui.add_space(8.0);
                    hint(ui, "새 버전에서 달라진 점", &p);
                    ScrollArea::vertical().id_salt("update-notes").max_height(notes_height).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(clipped_notes(&info.notes)).small()).wrap());
                    });
                }
                if action == Action::Restart {
                    ui.add_space(8.0);
                    hint(
                        ui,
                        "다시 시작하면 지금 문서가 새 창에 그대로 열립니다(저장하지 않은 내용과 커서 위치 포함, 실행 취소 기록은 제외). \
                         열려 있는 다른 mome 창은 각자 다시 시작하거나 닫았다 열면 새 버전이 됩니다.",
                        &p,
                    );
                }

                ui.add_space(8.0);
                hint(
                    ui,
                    "mome는 스스로 버전을 확인하러 나가지 않습니다. 이 창의 단추를 눌렀을 때만 GitHub(api.github.com, github.com)에 \
                     접속하며 보내는 정보는 없습니다. 내려받은 묶음은 GitHub가 알려 주는 확인값(SHA-256)과 견준 뒤에 설치하고, \
                     설정·임시 저장 기록은 그대로 둡니다.",
                    &p,
                );
            });

            // 덜 쓰는 단추들: 다시 확인, 릴리즈 페이지.
            let extras = |ui: &mut egui::Ui| {
                if up.busy.is_none() && action != Action::Check && secondary(ui, "다시 확인").clicked() {
                    return Some(Outcome::UpdateCheck);
                }
                let info = up.info.as_ref()?;
                let page = secondary(ui, "릴리즈 페이지").on_hover_text(&info.page);
                page.clicked().then(|| Outcome::OpenUrl(info.page.clone()))
            };
            ui.add_space(14.0);
            let first_row = button_row(ui, |ui| {
                // 다시 시작하는 중에는 닫지 않는다: 새 창이 뜨기 전에 고친 내용은 새 창으로 넘어가지 않는다.
                if ui.add_enabled(!up.is_restarting(), secondary_button("닫기")).clicked() {
                    return Some(Outcome::Close);
                }
                let label = match (up.busy, action) {
                    (Some(Busy::Checking), _) => "확인하는 중…".to_owned(),
                    (Some(Busy::Installing), _) => "설치하는 중…".to_owned(),
                    (Some(Busy::Restarting), _) => "다시 시작하는 중…".to_owned(),
                    (None, Action::Check) => "업데이트 확인".to_owned(),
                    (None, Action::Restart) => "지금 다시 시작".to_owned(),
                    (None, Action::Install) => {
                        // 버전은 위의 "최신 버전" 칸에 있다. 단추가 길면 한 줄에 다 들어가지 않는다.
                        format!("내려받아 설치 ({})", megabytes(up.info.as_ref().map_or(0, |info| info.size)))
                    }
                };
                if ui.add_enabled(up.busy.is_none(), primary_button(&label, &p)).clicked() {
                    return Some(match action {
                        Action::Check => Outcome::UpdateCheck,
                        Action::Install => Outcome::UpdateInstall,
                        Action::Restart => Outcome::UpdateRestart,
                    });
                }
                if two_rows {
                    None
                } else {
                    extras(ui)
                }
            });
            if two_rows && first_row.is_none() && up.info.is_some() {
                ui.add_space(6.0);
                return button_row(ui, extras);
            }
            first_row
        });
        answer(choice, dismissed && up.busy != Some(Busy::Restarting), Outcome::Close)
    }

    /// 최신 릴리즈를 확인한다(인터넷에 나간다).
    pub(in crate::app) fn start_update_check(&mut self, ctx: &Context) {
        if self.updater.busy.is_some() {
            return;
        }
        self.updater.spawn(ctx, Busy::Checking, |send| send(Event::Checked(update::check())));
    }

    /// 최신 릴리즈를 내려받아 실행 파일을 바꾼다.
    pub(in crate::app) fn start_update_install(&mut self, ctx: &Context) {
        if self.updater.busy.is_some() {
            return;
        }
        self.updater.spawn(ctx, Busy::Installing, |send| {
            let outcome = update::install_latest(&|stage| send(Event::Stage(stage)));
            send(Event::Installed(outcome));
        });
    }

    /// 설치한 새 버전으로 다시 시작한다: 지금 문서를 세션 기록에 남기고, 새 실행 파일을 그 기록으로 띄운 뒤,
    /// 새 창이 뜨면 이 창을 닫는다([`Self::poll_update`]). 저장할지 묻지 않는다: 저장하지 않은 내용은 새 창으로 넘어간다.
    pub(in crate::app) fn restart_for_update(&mut self, ctx: &Context) {
        if self.updater.busy.is_some() {
            return;
        }
        // 새 창이 읽을 기록에 가장 새 글과 창 위치를 담는다.
        self.track_window();
        if self.session_text_stale {
            self.copy_text_to_session();
        }
        self.sync_session(ctx);
        // 고치지 않은 문서는 새 창이 파일에서 다시 읽는다. 그 파일이 그사이에 없어졌거나 다른 프로그램이 바꿨으면
        // 화면의 내용과 달라지므로, 고친 문서처럼 지금 글을 넘긴다.
        let file_differs = self.doc.path.as_deref().is_some_and(|path| !path.exists()) || self.doc.changed_on_disk();
        if file_differs && !self.doc.is_modified() {
            self.session.update(|s| s.record.modified = true);
        }
        match self.session.save_for_restart() {
            Ok(written) => {
                // 임시 저장 폴더에 버전으로 남겼으면, 닫힐 때 같은 내용을 한 번 더 남기지 않는다.
                let in_history = match (&written, &self.settings.temp_dir) {
                    (Some(path), Some(root)) => path.starts_with(root),
                    _ => false,
                };
                if in_history {
                    self.snapshot.last_hash = Some(self.doc.current_hash);
                }
            }
            Err(err) => {
                log::error!("could not keep unsaved text for the restart: {err}");
                self.updater.error = Some(format!(
                    "저장하지 않은 내용을 새 창으로 넘길 수 없어 다시 시작하지 않았습니다({err}). \
                     문서를 저장한 뒤 다시 시도하거나, mome를 닫았다 열면 새 버전으로 열립니다."
                ));
                return;
            }
        }
        let sid = self.session.sid().to_owned();
        log::info!("restarting into the installed update (session {sid})");
        self.updater.spawn(ctx, Busy::Restarting, move |send| send(Event::Restarted(update::launch_new(&sid))));
    }

    /// 스레드가 알려 온 것을 받는다. 프레임마다 부른다(창이 최소화되어 있어도).
    pub(in crate::app) fn poll_update(&mut self, ctx: &Context) {
        let dialog_open = matches!(self.dialog, Some(Dialog::Update));
        while let Ok(event) = self.updater.rx.try_recv() {
            match event {
                Event::Checked(result) => {
                    self.updater.busy = None;
                    match result {
                        Ok(info) => self.updater.info = Some(info),
                        Err(error) => {
                            log::warn!("update check failed: {error}");
                            self.updater.error = Some(error);
                        }
                    }
                }
                Event::Stage(stage) => self.updater.stage = Some(stage),
                Event::Installed(result) => {
                    self.updater.busy = None;
                    self.updater.stage = None;
                    match result {
                        Ok(version) => {
                            if !dialog_open {
                                self.notify(format!(
                                    "mome {version} 설치를 마쳤습니다. [도움말 > mome 업데이트]에서 다시 시작하면 새 버전으로 열립니다."
                                ));
                            }
                            // 확인한 뒤에 더 새 릴리즈가 올라와 그것을 설치했을 수도 있다.
                            if let Some(info) = &mut self.updater.info {
                                if info.latest != version {
                                    info.latest.clone_from(&version);
                                    info.published.clear();
                                    info.notes.clear();
                                }
                            }
                            self.updater.installed = Some(version);
                        }
                        Err(error) => {
                            if !dialog_open {
                                self.notify_warning(format!("mome 업데이트를 설치하지 못했습니다: {error}"));
                            }
                            self.updater.error = Some(format!("설치하지 못했습니다: {error}"));
                        }
                    }
                }
                Event::Restarted(Ok(())) => {
                    // `busy`는 그대로 둔다: 닫히기까지의 몇 프레임 동안 단추가 다시 눌리지 않게.
                    log::info!("the new version is up; closing this window");
                    self.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                Event::Restarted(Err(error)) => {
                    self.updater.busy = None;
                    self.updater.error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(newer: bool, on_disk: bool, blocked: Option<&str>) -> UpdateInfo {
        UpdateInfo {
            latest: "0.1.9".to_owned(),
            newer,
            on_disk,
            published: "2026-11-23".to_owned(),
            notes: String::new(),
            page: update::RELEASES_PAGE.to_owned(),
            size: 7_500_000,
            blocked: blocked.map(str::to_owned),
        }
    }

    #[test]
    fn 상태에_맞는_단추와_안내를_고른다() {
        let mut up = Updater::default();
        assert_eq!((up.action(), up.verdict()), (Action::Check, None));
        assert_eq!(up.latest_text(), "아직 확인하지 않았습니다");

        up.info = Some(info(false, false, None));
        assert_eq!(up.action(), Action::Check);
        assert_eq!(up.verdict().as_deref(), Some("최신 버전을 쓰고 있습니다."));
        assert_eq!(up.latest_text(), "0.1.9 (2026-11-23)");

        up.info = Some(info(true, false, None));
        assert_eq!(up.action(), Action::Install);
        assert_eq!(up.verdict().as_deref(), Some("새 버전이 있습니다."));

        // 설치할 수 없는 자리에서는 까닭을 보여 주고 설치 단추를 내놓지 않는다.
        up.info = Some(info(true, false, Some("쓸 수 없습니다.")));
        assert_eq!(up.action(), Action::Check);
        assert_eq!(up.verdict().as_deref(), Some("새 버전이 있습니다. 쓸 수 없습니다."));

        // 다른 창이 이미 설치했으면 다시 시작만 하면 된다.
        up.info = Some(info(true, true, None));
        assert_eq!(up.action(), Action::Restart);
        assert!(up.verdict().unwrap().contains("다른 창에서 이미 설치되어 있습니다"));

        // 이 창이 설치했다(파일 속성을 읽지 못해도 안다).
        up.info = Some(info(true, false, None));
        up.installed = Some("0.1.9".to_owned());
        assert_eq!(up.action(), Action::Restart);
        assert!(up.verdict().unwrap().contains("설치를 마쳤습니다"));

        up.busy = Some(Busy::Installing);
        up.stage = Some(Stage::Download(1 << 20, 4 << 20));
        assert_eq!(up.verdict().as_deref(), Some("내려받는 중… 1.0MB / 4.0MB (25%)"));
        up.busy = Some(Busy::Checking);
        assert_eq!(up.latest_text(), "확인하는 중…");
    }

    #[test]
    fn 긴_릴리즈_설명은_앞부분만_보여_준다() {
        assert_eq!(clipped_notes("짧은 글"), "짧은 글");
        let long = "가".repeat(NOTES_LIMIT + 10);
        let clipped = clipped_notes(&long);
        assert_eq!(clipped.chars().count(), NOTES_LIMIT + 2);
        assert!(clipped.ends_with("\n…"));
    }
}
