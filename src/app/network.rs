//! 網路（開啟網址）的設定套用：HLS / DASH 畫質、重新連線、快取、逾時、憑證、標頭、proxy。
//! 對應到哪些 mpv 選項由 `net::mpv_options` 決定；啟動時同步設定（第一個網址就生效），之後非同步、只送有變的。
//! （「開啟網址」對話框、「設定 → 網路」頁之後加在這裡）

use super::VitascopeApp;
use crate::net::{self, NetSettings};

impl VitascopeApp {
    /// 啟動時（還沒開檔）同步套用：系統的 libmpv 0.37 預設不檢查網站憑證，從第一個網址就要檢查
    pub(super) fn net_startup(&mut self) {
        self.net_defaults = self.player.net_defaults();
        let opts = net::mpv_options(&self.settings.net, &self.net_defaults);
        for (name, _, result) in self.player.apply_net(&opts, true) {
            if let Err(e) = result {
                eprintln!("[vitascope] 無法套用 {name}：{e}");
            }
        }
    }

    /// 設定改了之後：非同步送出有變的選項（播放中改不會卡住介面；正在播的串流到下一次連線才用新的值）
    fn apply_net(&mut self) {
        let opts = net::mpv_options(&self.settings.net, &self.net_defaults);
        for (name, key, result) in self.player.apply_net(&opts, false) {
            match result {
                Ok(Some(id)) => {
                    self.async_pending.insert(id, name.to_owned());
                }
                Ok(None) => {}
                Err(e) => self.async_failed(key, name, &e.to_string()),
            }
        }
    }

    /// 改網路設定：整理過（去掉換行之類）、有變才套用、存檔。「設定 → 網路」頁用；測試也直接呼叫
    #[doc(hidden)]
    pub fn change_net(&mut self, change: impl FnOnce(&mut NetSettings)) {
        let before = self.settings.net.clone();
        change(&mut self.settings.net);
        self.settings.net = std::mem::take(&mut self.settings.net).sanitized();
        if self.settings.net != before {
            self.apply_net();
            self.save_settings();
        }
    }
}
