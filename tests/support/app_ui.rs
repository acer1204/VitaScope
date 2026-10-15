//! 介面測試（egui_kittest 的 `VitascopeApp`）共用的工具：右鍵選單（捲到看得到、等子選單打開）、
//! 「網站影片 ▸ 選擇畫質」、每一幀記下 OSD 與送出的視窗大小（`Seen`）。
//! tests/ui.rs 與 tests/net_errors.rs（換畫質時網址失效的介面測試）共用

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::time::{Duration, Instant};
use vitascope::app::VitascopeApp;
use vitascope::player::TrackKind;

/// 網路測試等久一點（CI 的電腦比較慢；連線、緩衝的時間跟網路的速度有關）
pub const NET_TIMEOUT: Duration = Duration::from_secs(30);

/// 選單、子選單的項目最多等這麼久（CI 的電腦比較慢；正常的話幾幀就好）
pub const MENU_TIMEOUT: Duration = Duration::from_secs(30);

/// 等視窗配合影片調整完最多這麼久（同 tests/ui.rs 的 `TIMEOUT`）
const FIT_TIMEOUT: Duration = Duration::from_secs(10);

/// 這一幀送給視窗的指令
pub fn viewport_commands(h: &Harness<'_, VitascopeApp>) -> Vec<egui::ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// 等待時每一幀看到的 OSD、送出的視窗大小（OSD 1.5 秒就消失：CI 比較慢時等完再看可能已經不見了）
#[derive(Default)]
pub struct Seen {
    pub osd: Vec<String>,
    pub sizes: Vec<egui::Vec2>,
}

impl Seen {
    pub fn record(&mut self, h: &Harness<'_, VitascopeApp>) {
        if let Some(t) = h.state().osd_text()
            && self.osd.last().map(String::as_str) != Some(t)
        {
            self.osd.push(t.to_owned());
        }
        self.sizes
            .extend(viewport_commands(h).into_iter().filter_map(|c| match c {
                egui::ViewportCommand::InnerSize(s) => Some(s),
                _ => None,
            }));
    }

    /// 最後看到的提示（等的時間比提示顯示的 1.5 秒長時，提示可能已經消失了：看的是消失前的那一個）
    pub fn last_osd(&self) -> Option<&str> {
        self.osd.last().map(String::as_str)
    }
}

/// 跑介面幀直到條件成立（等 `NET_TIMEOUT`，同 tests/ui.rs 的 `step_until_net`），每一幀記下 OSD、視窗大小
pub fn step_until_seen(
    h: &mut Harness<'_, VitascopeApp>,
    seen: &mut Seen,
    what: &str,
    cond: impl Fn(&VitascopeApp) -> bool,
) {
    let start = Instant::now();
    seen.record(h);
    while !cond(h.state()) {
        assert!(
            start.elapsed() < NET_TIMEOUT,
            "等待逾時：{what}
目前狀態：{:#?}",
            h.state().player().state
        );
        h.step();
        seen.record(h);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 在影片上按右鍵、捲到選單裡的項目、把滑鼠移到它上面（有子選單的話子選單會打開）
pub fn hover_context_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    wait_window_fit(h);
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    hover_menu_item(h, label);
}

/// 選單已經打開：等到項目出現、捲到看得到、把滑鼠移到它上面。有子選單的項目（標籤以 ⏵ 結尾、沒停用）
/// 等到子選單打開（見 `MenuItem::hover`）
pub fn hover_menu_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    MenuItem::contains(label).hover(h);
}

/// 視窗還要配合影片調整的話，等它調整完（畫面設定好才調整，CI 上可能晚好幾幀）。
/// 選單開著時視窗改大小，選單跟著移動、項目可能跑到視窗外，滑鼠就不在要點的項目上了
pub fn wait_window_fit(h: &mut Harness<'_, VitascopeApp>) {
    if !window_fit_coming(h.state()) {
        apply_pending_resize(h);
        return;
    }
    let start = Instant::now();
    while window_fit_coming(h.state()) {
        assert!(start.elapsed() < FIT_TIMEOUT, "等待逾時：視窗配合影片調整完");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    apply_pending_resize(h);
}

/// 調整視窗的那一幀送出新的大小，下一幀才照新的大小排版：還沒排的話跑一幀（之後點的位置、看得到的範圍才對）
pub fn apply_pending_resize(h: &mut Harness<'_, VitascopeApp>) {
    if h.input().screen_rect.is_some() {
        h.step();
    }
}

/// 開檔之後視窗還會配合影片調整：已經知道影片尺寸，或檔案載入好了、選的是影像軌（或還不知道選了哪幾軌）。
/// 只看影片尺寸的話，載入好、還不知道尺寸時（CI 上可能晚好幾幀）會不等，選單打開後視窗才變小。
/// 沒有影像（純音訊、只播聲音）不會調整；還在連線、載入時也不等（測試可能就是要在載入中操作）
pub fn window_fit_coming(app: &VitascopeApp) -> bool {
    let s = &app.player().state;
    let selection_known = s.tracks.iter().any(|t| t.selected);
    app.window_fit_pending()
        && (s.video_size.is_some() || (s.loaded && (!selection_known || s.selected(TrackKind::Video).is_some())))
}

/// 等到子選單打開、裡面有這一項（CI 比較慢：滑鼠移過去之後子選單不一定下一幀就打開）
pub fn wait_menu_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    let start = Instant::now();
    while h.query_all_by_label(label).next().is_none() {
        assert!(start.elapsed() < MENU_TIMEOUT, "等不到選單裡的「{label}」");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 在影片上按右鍵、打開「網站影片 ▸」，等它的子選單出來
pub fn open_site_menu(h: &mut Harness<'_, VitascopeApp>) {
    hover_context_item(h, "網站影片");
    wait_menu_item(h, "選擇畫質 ⏵");
}

/// 網站影片 ▸ 選擇畫質 ▸ 某一項
pub fn pick_site_quality(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    open_site_menu(h);
    hover_menu_item(h, "選擇畫質");
    wait_menu_item(h, label);
    h.get_by_label(label).click();
    h.step();
}

/// 選單裡的一個項目：`exact` = 標籤剛好是 `label`，不然是標籤含 `label`
#[derive(Clone, Copy)]
pub struct MenuItem<'a> {
    label: &'a str,
    exact: bool,
}

impl<'a> MenuItem<'a> {
    pub fn exact(label: &'a str) -> Self {
        Self { label, exact: true }
    }

    pub fn contains(label: &'a str) -> Self {
        Self { label, exact: false }
    }

    /// 現在畫面上的這一項（無障礙樹的節點）。還沒畫出來時 None
    pub fn node<'h>(self, h: &'h Harness<'_, VitascopeApp>) -> Option<egui_kittest::Node<'h>>
    where
        'a: 'h,
    {
        if self.exact {
            h.query_by_label(self.label)
        } else {
            h.query_by_label_contains(self.label)
        }
    }

    /// 現在畫面上的這一項：範圍、完整的標籤、停用了沒。還沒畫出來時 None
    pub fn find(self, h: &Harness<'_, VitascopeApp>) -> Option<(egui::Rect, String, bool)> {
        let node = self.node(h)?;
        let a = node.accesskit_node();
        Some((node.rect(), a.label().unwrap_or_default().to_string(), a.is_disabled()))
    }

    /// egui 記下的這一項：`rect` 是整個項目，`interact_rect` 是滑鼠碰得到的部分（被選單的捲動區域裁掉之後），
    /// `contains_pointer()` = 滑鼠在它上面、沒被別的圖層（例如子選單）蓋住（停用的項目也算）。
    /// 兩幀之間 `read_response` 讀到的是再前一幀的（egui 在一幀結束時才把它換成「上一幀」）：
    /// 範圍跟無障礙樹（最新的一幀）一樣才算數，不一樣（剛移動、剛捲動）時 None
    pub fn response(self, h: &Harness<'_, VitascopeApp>) -> Option<egui::Response> {
        let node = self.node(h)?;
        let now = node.rect();
        let r = h.ctx.read_response(widget_id(&node))?;
        ((r.rect.min - now.min).length() < 0.5 && (r.rect.max - now.max).length() < 0.5).then_some(r)
    }

    /// 整個項目都看得到：在視窗裡，也沒被選單的捲動區域裁掉。egui 記的還不是最新的一幀時 None（再跑一幀）。
    /// 只看在不在視窗裡不夠：選單有邊框，從按右鍵的地方往下開（放不下才往上移），捲動區域常比視窗小一截，
    /// 被裁掉的項目也在視窗裡，滑鼠移到它的中心卻碰不到它
    pub fn shown(self, h: &Harness<'_, VitascopeApp>) -> Option<bool> {
        let r = self.response(h)?;
        Some(h.ctx.content_rect().contains_rect(r.rect) && r.interact_rect.expand(0.5).contains_rect(r.rect))
    }

    /// egui 算滑鼠在這一項上面（點下去會點到它）
    pub fn under_pointer(self, h: &Harness<'_, VitascopeApp>) -> bool {
        self.response(h).is_some_and(|r| r.contains_pointer())
    }

    /// 請 egui 把這一項捲到它所在的捲動區域中間：無障礙的「捲到看得到」（AccessKit 的 ScrollIntoView，螢幕閱讀器用的），
    /// 跟滑鼠在哪裡無關
    pub fn scroll_into_view(self, h: &mut Harness<'_, VitascopeApp>) {
        let Some((target_node, target_tree)) = self.node(h).map(|n| n.accesskit_node().locate()) else {
            return;
        };
        h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::ScrollIntoView,
            target_tree,
            target_node,
            data: None,
        }));
    }

    /// 等到這一項出現（子選單不一定下一幀就打開：CI 比較慢，選單也可能因為視窗剛改完大小換了位置）
    pub fn wait(self, h: &mut Harness<'_, VitascopeApp>) -> (egui::Rect, String, bool) {
        let start = Instant::now();
        loop {
            if let Some(found) = self.find(h) {
                return found;
            }
            assert!(start.elapsed() < MENU_TIMEOUT, "等不到選單裡的「{}」", self.label);
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 等到這一項出現、整個看得到（選單比視窗長時捲動選單）、停下來，回傳它現在的範圍。
    /// 每次都重新看：視窗可能剛配合影片改小，原本看得到的項目跑到視窗外。
    /// 不用滾輪捲：滾輪要滑鼠停在選單上，項目從滑鼠下面滑過，滑過有子選單（⏵）的項目就打開它的子選單。
    /// 窄的視窗（迷你播放器 480 寬）兩邊都放不下子選單，egui 把它疊在選單上、蓋住滑鼠，之後的滾輪都捲到子選單，
    /// 選單不動，要點的項目一直在看不到的地方。哪個項目剛好停在滑鼠下面看選單的內容與上次捲到哪裡：
    /// 本機剛好沒事，windows-latest 上迷你播放器的測試點不到「回到一般視窗」「結束影戲」、打不開「視窗置頂」
    pub fn bring_into_view(self, h: &mut Harness<'_, VitascopeApp>) -> egui::Rect {
        let start = Instant::now();
        loop {
            apply_pending_resize(h);
            let (before, ..) = self.wait(h);
            match self.shown(h) {
                Some(true) => {
                    // 停下來才算（選單剛打開、視窗剛改大小時，位置還會變一兩幀）
                    h.step();
                    let (now, ..) = self.wait(h);
                    if now == before && self.shown(h) == Some(true) {
                        return now;
                    }
                }
                Some(false) => {
                    self.scroll_into_view(h);
                    h.run_steps(2);
                }
                None => h.step(),
            }
            assert!(
                start.elapsed() < MENU_TIMEOUT,
                "「{}」一直捲不到看得到的地方（範圍, 碰得到的部分）：{:?}",
                self.label,
                self.response(h).map(|r| (r.rect, r.interact_rect))
            );
        }
    }

    /// 等到這一項出現、捲到看得到、等它停下來，把滑鼠移到它上面，而且 egui 也算滑鼠在它上面
    /// （版面剛好變了就再來一次；一直不是就在這裡失敗，不要等到點下去沒反應才發現）。有子選單的項目等到子選單打開
    pub fn hover(self, h: &mut Harness<'_, VitascopeApp>) {
        // 之後用完整的標籤找：子選單打開後可能有別的項目也含 `label`（例如「字幕 ⏵」裡的「第二字幕 ⏵」）
        let (_, full, disabled) = self.wait(h);
        let me = MenuItem::exact(&full);
        let submenu = full.ends_with('⏵') && !disabled;
        let start = Instant::now();
        loop {
            let item = me.bring_into_view(h);
            me.point_at(h, item);
            // 有子選單的項目：窄的視窗裡它的子選單可能疊在它上面（這時滑鼠在子選單上）
            if me.under_pointer(h) || (submenu && submenu_open(h, &full)) {
                break;
            }
            assert!(
                start.elapsed() < MENU_TIMEOUT,
                "滑鼠移到「{full}」上，egui 卻不算在它上面（被別的子選單蓋住了？）"
            );
        }
        if submenu {
            me.wait_submenu(h);
        }
    }

    /// 把滑鼠移到 `item`（這一項剛才的範圍）上，多跑幾幀（有子選單的項目，子選單要一兩幀才打開）
    pub fn point_at(self, h: &mut Harness<'_, VitascopeApp>, item: egui::Rect) {
        h.event(egui::Event::PointerMoved(item.center()));
        h.run_steps(3);
    }

    /// 滑鼠已經移到這一項（有子選單、標籤要完整）上：等到它的子選單打開。等的時候每隔幾幀再捲一次、移過去一次：
    /// 版面變了（例如選單打開後視窗才配合影片變小，項目跑到視窗外），滑鼠已經不在它上面
    pub fn wait_submenu(self, h: &mut Harness<'_, VitascopeApp>) {
        let start = Instant::now();
        let mut frames = 0;
        while !submenu_open(h, self.label) {
            assert!(start.elapsed() < MENU_TIMEOUT, "「{}」的子選單一直沒打開", self.label);
            frames += 1;
            if frames % 10 == 0 {
                let header = self.bring_into_view(h);
                h.event(egui::Event::PointerMoved(header.center()));
            }
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// 無障礙樹的節點對應的 egui 代號（`Id::accesskit_id` 的反過來）
pub fn widget_id(node: &egui_kittest::Node<'_>) -> egui::Id {
    let (local, _) = node.accesskit_node().locate();
    // SAFETY: 不是記憶體安全的問題：值本來就是 egui 的代號
    unsafe { egui::Id::from_high_entropy_bits(local.0) }
}

/// 標籤是 `header` 的項目的子選單開著（這一幀畫了它的子選單）。
/// 子選單是畫面最上層的一個區域，裡面那一層的代號是 egui 從項目的代號算出來的（`SubMenu::id_from_widget_id`）
pub fn submenu_open(h: &Harness<'_, VitascopeApp>, header: &str) -> bool {
    let Some(item) = h.query_by_label(header) else {
        return false;
    };
    let submenu = egui::containers::menu::SubMenu::id_from_widget_id(widget_id(&item)).accesskit_id();
    h.root()
        .children()
        .any(|area| area.children().any(|ui| ui.accesskit_node().locate().0 == submenu))
}
