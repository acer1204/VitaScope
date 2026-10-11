//! 播放清單：開一個檔案時，同資料夾裡同類的檔案（影片或音樂）依檔名排序成清單，
//! 播完自動接下一個，PgUp / PgDn 切換上一個 / 下一個（比照 PotPlayer）。
//! 一次拖放多個檔案時，清單就是那幾個檔案。
//!
//! 播放順序（隨機、重複）也在這裡，全部是純邏輯：mpv 的清單永遠只有一項，接下來播哪一個由這裡決定。
//! 隨機播放是清單位置的一個排列（`ShuffleOrder`），編輯清單時跟著更新，所以「這一輪播過哪些」不會亂掉。

use crate::formats;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 重複播放（存檔）。單一檔案由 mpv 的 `loop-file` 重播；全部是清單播完後從頭（隨機時重新洗牌）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Repeat {
    #[default]
    Off,
    One,
    All,
}

/// 播放順序（存檔）：隨機、重複。隨機的順序本身不存，下次開啟時重新洗牌
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayOrder {
    pub shuffle: bool,
    pub repeat: Repeat,
}

/// 洗牌用的亂數（SplitMix64）：不用另外加 rand。不存在 [`Playlist`] 裡、由呼叫的一方傳進來，
/// 清單才能維持 `Clone + Default`；測試給固定的種子，結果每次都一樣
#[derive(Debug, Clone)]
pub struct Shuffler(u64);

impl Shuffler {
    pub fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    /// 用現在的時間和行程編號當種子：同時開兩個影戲也不會洗出一樣的順序
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self(nanos ^ u64::from(std::process::id()).rotate_left(32))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `0..n` 裡的一個數（`n` 是 0 時是 0）。用乘法取高位（Lemire），偏差小到可以忽略
    pub fn below(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        ((u128::from(self.next_u64()) * n as u128) >> 64) as usize
    }

    /// Fisher–Yates 洗牌
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// 隨機播放的順序：清單位置（`items` 的索引）的一個排列。
/// `order[..pos]` 是這一輪已經播過的，`order[pos]` 是目前的檔案。目前的檔案已經移出清單
/// （或清單還原、清空後還沒開始播）時，`order[pos]` 是「下一個」，這時 `pos` 可以等於長度（沒有下一個）
#[derive(Debug, Clone, Default)]
struct ShuffleOrder {
    order: Vec<usize>,
    pos: usize,
}

impl ShuffleOrder {
    /// 洗出新的一輪：`first`（目前的檔案）排第一個、其餘打散，從頭開始
    fn new(len: usize, first: Option<usize>, rng: &mut Shuffler) -> Self {
        let first = first.filter(|&k| k < len);
        let mut rest: Vec<usize> = (0..len).filter(|&k| Some(k) != first).collect();
        rng.shuffle(&mut rest);
        Self {
            order: first.into_iter().chain(rest).collect(),
            pos: 0,
        }
    }

    /// 清單位置 `k` 在順序裡的哪一格（是一個排列，一定找得到）
    fn slot_of(&self, k: usize) -> usize {
        self.order.iter().position(|&e| e == k).unwrap_or(self.order.len())
    }

    /// 每一項換成新的清單位置（拖曳、排序：項目沒變，只是位置變了）
    fn remap(&mut self, new_of: impl Fn(usize) -> usize) {
        for e in &mut self.order {
            *e = new_of(*e);
        }
    }

    /// 把新的項目放進還沒播的部分（`order[lo..]`）。
    /// 有亂數時跟原本未播的項目隨機穿插（等於一個一個插到隨機的位置），原本未播的項目彼此的順序不變；
    /// 沒有亂數時照原本的順序放在 `lo`（`at_lo`）或最後
    fn place_unplayed(&mut self, lo: usize, mut new: Vec<usize>, rng: Option<&mut Shuffler>, at_lo: bool) {
        let lo = lo.min(self.order.len());
        let Some(rng) = rng else {
            let at = if at_lo { lo } else { self.order.len() };
            self.order.splice(at..at, new);
            return;
        };
        rng.shuffle(&mut new);
        // 一次合併：一千個檔案加進上千項的清單也不會一個一個 insert（每次都要搬整個陣列）
        let old: Vec<usize> = self.order.split_off(lo);
        let (mut old_left, mut new_left) = (old.len(), new.len());
        let (mut old, mut new) = (old.into_iter(), new.into_iter());
        while old_left + new_left > 0 {
            if rng.below(old_left + new_left) < new_left {
                self.order.extend(new.next());
                new_left -= 1;
            } else {
                self.order.extend(old.next());
                old_left -= 1;
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Playlist {
    items: Vec<PathBuf>,
    /// 目前的檔案；目前的檔案被移出清單時，是原本接在它後面的那一項（隨機播放時是順序裡的下一個）
    index: usize,
    /// 目前播放的檔案已經從清單移除（繼續播，下一個是 `index` 那一項）
    current_removed: bool,
    /// 使用者自己整理的清單（拖放多個檔案、開啟清單檔、在面板上編輯過）：關閉程式時存起來。
    /// 同資料夾掃描出來的清單下次開檔時會再掃，不用存
    manual: bool,
    /// 隨機播放的順序；沒開隨機播放時是 None。
    /// 開著的時候一直保持：`order` 是 `0..len` 的排列，而且 `index` 就是 `order[pos]`（沒有下一個時是 `len`）
    shuffle: Option<ShuffleOrder>,
}

impl Playlist {
    /// `path` 所在資料夾裡、跟它同類的檔案。讀不到資料夾時，清單只有這個檔案
    pub fn for_file(path: &Path) -> Self {
        let kind = formats::media_kind(path);
        let mut items: Vec<PathBuf> = path
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|entries| {
                entries
                    .flatten()
                    // 先看副檔名，不是同類的檔案就不用再查檔案類型（網路磁碟上每查一次都慢）
                    .filter(|e| kind.is_some() && formats::media_kind(&e.path()) == kind)
                    .filter(|e| !is_hidden(e))
                    .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        sort_by_name(&mut items);
        let mut list = Self::from_files(items);
        if !list.select(path) {
            // 副檔名不在清單上之類的情況：至少包含目前的檔案
            list = Self::from_files(vec![path.to_path_buf()]);
        }
        list
    }

    /// 指定的檔案（例如一次拖放的多個檔案），維持原本的順序
    pub fn from_files(items: Vec<PathBuf>) -> Self {
        Self {
            items,
            index: 0,
            current_removed: false,
            manual: false,
            shuffle: None,
        }
    }

    /// 標成使用者自己整理的清單
    pub fn manual(mut self) -> Self {
        self.manual = true;
        self
    }

    pub fn is_manual(&self) -> bool {
        self.manual
    }

    /// 清空後的清單：還沒有在播的項目，之後加進來的第一個就是「下一個」
    pub fn cleared() -> Self {
        Self::restored(Vec::new(), None)
    }

    /// 上次存下的清單：還沒開始播，「下一個」是上次播的那一項
    pub fn restored(items: Vec<PathBuf>, current: Option<usize>) -> Self {
        Self {
            index: current.unwrap_or(0).min(items.len().saturating_sub(1)),
            items,
            current_removed: true,
            manual: true,
            shuffle: None,
        }
    }

    // ───────────── 隨機播放 ─────────────

    pub fn is_shuffled(&self) -> bool {
        self.shuffle.is_some()
    }

    /// 開關隨機播放。打開時目前的檔案排第一個（還沒開始播、或目前的已經移出清單時，「下一個」排第一個），
    /// 其餘打散，每一項這一輪播一次；已經開著時不重新洗牌（播過哪些照舊）。
    /// 關掉時回到清單的順序，從目前的檔案接下去
    pub fn set_shuffle(&mut self, on: bool, rng: &mut Shuffler) {
        if !on {
            self.shuffle = None;
            return;
        }
        if self.shuffle.is_some() {
            return;
        }
        self.shuffle = Some(ShuffleOrder::new(self.items.len(), Some(self.index), rng));
        self.sync_index();
        self.debug_check();
    }

    /// 整份清單一起開（.m3u、一次拖放多個檔案、加入檔案到空的播放器）而且開著隨機播放：
    /// 整份重新洗牌，連第一個也是隨機的。回傳要先播的那一項（之後一樣用 [`Self::select_index`] 開它）；
    /// 清單是空的時回傳 None。開單一檔案、雙擊、還原上次的清單則用 [`Self::set_shuffle`]，從那個檔案開始
    pub fn shuffle_whole_list(&mut self, rng: &mut Shuffler) -> Option<usize> {
        self.shuffle = Some(ShuffleOrder::new(self.items.len(), None, rng));
        // 還沒開始播：「下一個」是新順序的第一個
        self.current_removed = true;
        self.sync_index();
        self.debug_check();
        self.next_index()
    }

    /// 測試、除錯用：隨機播放的順序和目前在第幾格
    #[doc(hidden)]
    pub fn shuffle_order(&self) -> Option<(&[usize], usize)> {
        self.shuffle.as_ref().map(|s| (s.order.as_slice(), s.pos))
    }

    /// 隨機播放時 `index` 一律從順序讀出來（目前的檔案，或目前的已經移出清單時是下一個）
    fn sync_index(&mut self) {
        if let Some(s) = &self.shuffle {
            self.index = s.order.get(s.pos).copied().unwrap_or(self.items.len());
        }
    }

    /// 除錯版每次改動後都檢查隨機順序的不變條件（測試就是除錯版）
    fn debug_check(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        let Some(s) = &self.shuffle else {
            return;
        };
        let len = self.items.len();
        assert_eq!(s.order.len(), len, "隨機順序的長度跟清單不一樣：{self:?}");
        let mut seen = vec![false; len];
        for &k in &s.order {
            assert!(k < len && !seen[k], "隨機順序不是排列：{self:?}");
            seen[k] = true;
        }
        if len == 0 {
            assert_eq!(s.pos, 0, "{self:?}");
        } else if self.current_removed {
            assert!(s.pos <= len, "{self:?}");
            assert_eq!(self.index, s.order.get(s.pos).copied().unwrap_or(len), "{self:?}");
        } else {
            assert!(s.pos < len, "{self:?}");
            assert_eq!(self.index, s.order[s.pos], "{self:?}");
        }
    }

    // ───────────── 選擇、上一個 / 下一個 ─────────────

    /// 把 `path` 設成目前的檔案；不在清單上就回傳 false。
    /// 隨機播放時這是「跳到」那一項（從最近開啟的檔案、拖放之類的地方開）：播過的留在它前面，還沒播的排在它後面
    pub fn select(&mut self, path: &Path) -> bool {
        // 同一個檔案在清單上出現兩次時，優先用目前那一項（不然雙擊第二個會跳回第一個）
        if !self.current_removed && self.items.get(self.index).is_some_and(|p| same_file(p, path)) {
            return true;
        }
        match self.items.iter().position(|p| same_file(p, path)) {
            Some(i) => {
                if let Some(s) = &mut self.shuffle {
                    jump(s, i, self.current_removed);
                }
                self.index = i;
                self.current_removed = false;
                self.debug_check();
                true
            }
            None => false,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.items.iter().any(|p| same_file(p, path))
    }

    pub fn current(&self) -> Option<&Path> {
        if self.current_removed {
            return None;
        }
        self.items.get(self.index).map(PathBuf::as_path)
    }

    /// 目前的檔案在清單上的位置（從 0 開始）；已經移出清單時是 None
    pub fn current_index(&self) -> Option<usize> {
        (!self.current_removed && self.index < self.items.len()).then_some(self.index)
    }

    /// 直接選第 `i` 項（從清單上點、上一個 / 下一個）：同一個檔案出現兩次時才不會跳回第一個。
    /// 隨機播放時，「一步」跟「跳」分開：`i` 是順序裡的下一個、上一個（或目前的已經移出清單、`i` 是下一個）時
    /// 只移動位置，所以 PgUp 一直按會沿著播過的順序往回走；其他的項目（雙擊清單上別的檔案）才是「跳」，
    /// 規則跟 [`Self::select`] 一樣
    pub fn select_index(&mut self, i: usize) -> bool {
        if i >= self.items.len() {
            return false;
        }
        if let Some(s) = &mut self.shuffle {
            let next_slot = if self.current_removed { s.pos } else { s.pos + 1 };
            if s.order.get(next_slot) == Some(&i) {
                s.pos = next_slot;
            } else if s.pos.checked_sub(1).and_then(|p| s.order.get(p)) == Some(&i) {
                s.pos -= 1;
            } else if self.current_removed || s.order.get(s.pos) != Some(&i) {
                jump(s, i, self.current_removed);
            }
        }
        self.index = i;
        self.current_removed = false;
        self.debug_check();
        true
    }

    /// 下一個 / 上一個在清單上的位置。隨機播放時照洗好的順序：下一個是這一輪還沒播的下一項，
    /// 上一個是這一輪前一個播的（PgUp 沿著播過的順序往回走）
    pub fn next_index(&self) -> Option<usize> {
        if let Some(s) = &self.shuffle {
            let slot = if self.current_removed { s.pos } else { s.pos + 1 };
            return s.order.get(slot).copied();
        }
        let next = if self.current_removed {
            self.index
        } else {
            self.index + 1
        };
        (next < self.items.len()).then_some(next)
    }

    pub fn prev_index(&self) -> Option<usize> {
        if let Some(s) = &self.shuffle {
            return s.pos.checked_sub(1).and_then(|p| s.order.get(p).copied());
        }
        self.index.checked_sub(1).filter(|i| *i < self.items.len())
    }

    /// 重複全部時，有沒有別的項目可以繞回去（清單是空的、或只有目前這一個時沒有：重播目前的檔案）
    pub fn can_wrap(&self) -> bool {
        match self.items.len() {
            0 => false,
            1 => self.current_removed,
            _ => true,
        }
    }

    /// 重複全部：清單播完後，下一輪的第一個（之後一樣用 [`Self::select_index`] 開它）。
    /// 照清單順序時是第一項；隨機時重新洗牌，剛播完的那一個排第一格、算這一輪播過了，不會馬上又播一次。
    /// 沒有別的項目可以繞回去時是 None（見 [`Self::can_wrap`]），這時重播目前的檔案
    pub fn wrap_index(&mut self, rng: &mut Shuffler) -> Option<usize> {
        if !self.can_wrap() {
            return None;
        }
        if self.shuffle.is_some() {
            let finished = (!self.current_removed).then_some(self.index);
            self.shuffle = Some(ShuffleOrder::new(self.items.len(), finished, rng));
            self.sync_index();
            self.debug_check();
            return self.next_index();
        }
        Some(0)
    }

    /// 手動繞回去的目標（照清單順序）：重複全部時，最後一項按 PgDn 到第一項、第一項按 PgUp 到最後一項。
    /// 隨機播放時沒有（往後用 [`Self::wrap_index`] 重新洗牌；往前到了這一輪的開頭就是第一個）；
    /// 沒有別的項目可以繞回去時也沒有
    pub fn first_index(&self) -> Option<usize> {
        (self.shuffle.is_none() && self.can_wrap()).then_some(0)
    }

    pub fn last_index(&self) -> Option<usize> {
        (self.shuffle.is_none() && self.can_wrap()).then(|| self.items.len() - 1)
    }

    /// 存檔用的位置：正在播的那一項；已經移出清單（或還原後還沒開始播）時是接下來要播的那一項
    pub fn resume_index(&self) -> Option<usize> {
        (self.index < self.items.len()).then_some(self.index)
    }

    pub fn next(&self) -> Option<&Path> {
        self.next_index().map(|i| self.items[i].as_path())
    }

    pub fn prev(&self) -> Option<&Path> {
        self.prev_index().map(|i| self.items[i].as_path())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 目前是第幾個（從 1 開始），清單上的位置
    pub fn position(&self) -> usize {
        self.index + 1
    }

    /// 播放順序裡的第幾個（從 1 開始）：隨機播放時是「這一輪的第幾個」，否則跟 [`Self::position`] 一樣
    pub fn order_position(&self) -> usize {
        match &self.shuffle {
            Some(s) => s.pos + 1,
            None => self.position(),
        }
    }

    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    // ───────────── 編輯 ─────────────

    /// 移出清單（不動檔案）。移掉的是目前的檔案時，照樣繼續播，下一個是原本接在它後面的
    pub fn remove(&mut self, i: usize) {
        if i >= self.items.len() {
            return;
        }
        self.manual = true;
        self.items.remove(i);
        if let Some(s) = &mut self.shuffle {
            let slot = s.slot_of(i);
            s.order.remove(slot);
            s.remap(|k| if k > i { k - 1 } else { k });
            if slot < s.pos {
                s.pos -= 1;
            } else if slot == s.pos && !self.current_removed {
                // 移掉的是目前的檔案：pos 不動，現在指著原本的下一個
                self.current_removed = true;
            }
            // 目前的已經移出清單、又移掉「下一個」時，pos 一樣不動，指著再下一個；index 要重新讀
            self.sync_index();
        } else if i < self.index {
            self.index -= 1;
        } else if i == self.index && !self.current_removed {
            self.current_removed = true;
        }
        self.debug_check();
    }

    /// 把第 `from` 項移到第 `to` 項的位置（拖曳排序）；`to` 是移動之前的位置，可以等於 `len()`（放到最後）
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to > self.items.len() || from == to || from + 1 == to {
            return;
        }
        self.manual = true;
        let item = self.items.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.items.insert(to, item);
        // 每一項移動後的位置。目前那一項（目前的檔案已經移出清單時是「下一個」）跟著原本的那一項走：
        // 用位置算、不用檔名找，同一個檔案在清單上出現兩次時才不會跳到另一個
        let moved = |k: usize| {
            if k == from {
                return to;
            }
            let k = if k > from { k - 1 } else { k };
            if k >= to { k + 1 } else { k }
        };
        match &mut self.shuffle {
            // 隨機的順序記的是項目：位置換了，順序和播過哪些都不變
            Some(s) => {
                s.remap(moved);
                self.sync_index();
            }
            None => self.index = moved(self.index),
        }
        self.debug_check();
    }

    /// 加到清單最後（已經在清單上的不重複加）；回傳加了幾個。
    /// 隨機播放時新的項目照加入的順序排在順序的最後（還是這一輪還沒播的部分）；要打散用 [`Self::extend_with`]
    pub fn extend(&mut self, files: impl IntoIterator<Item = PathBuf>) -> usize {
        self.add(files, None)
    }

    /// 同 [`Self::extend`]，隨機播放時新的項目打散到這一輪還沒播的部分
    pub fn extend_with(&mut self, files: impl IntoIterator<Item = PathBuf>, rng: &mut Shuffler) -> usize {
        self.add(files, Some(rng))
    }

    /// 用雜湊表比對：加一個上千個檔案的資料夾時，逐一比對會卡住畫面好幾秒
    fn add(&mut self, files: impl IntoIterator<Item = PathBuf>, rng: Option<&mut Shuffler>) -> usize {
        self.manual = true;
        let before = self.items.len();
        let mut seen: HashSet<String> = self.items.iter().map(|p| file_key(p)).collect();
        for f in files {
            if seen.insert(file_key(&f)) {
                self.items.push(f);
            }
        }
        let after = self.items.len();
        if let Some(s) = &mut self.shuffle {
            // 還沒播的部分：目前的檔案已經移出清單（清空、還原後還沒開始播）時從 pos 開始，否則從 pos 的下一格
            let lo = if self.current_removed { s.pos } else { s.pos + 1 };
            s.place_unplayed(lo, (before..after).collect(), rng, false);
            // 目前的已經移出清單時，新的項目可能排在 pos，成為「下一個」
            self.sync_index();
        }
        self.debug_check();
        after - before
    }

    /// 目前那一項換成 `items`（網路上的播放清單展開成裡面的項目），回傳第一個換進來的在清單上的位置；
    /// 沒有目前的項目時不動、回傳 None。換完之後目前的是第一個換進來的（之後由開檔選要播的那一個）；
    /// `items` 是空的時跟移出清單一樣。隨機播放時其餘換進來的照順序排在目前的後面；要打散用
    /// [`Self::replace_current_with`]
    pub fn replace_current(&mut self, items: Vec<PathBuf>) -> Option<usize> {
        self.replace(items, None)
    }

    /// 同 [`Self::replace_current`]，隨機播放時其餘換進來的打散到這一輪還沒播的部分
    pub fn replace_current_with(&mut self, items: Vec<PathBuf>, rng: &mut Shuffler) -> Option<usize> {
        self.replace(items, Some(rng))
    }

    fn replace(&mut self, items: Vec<PathBuf>, rng: Option<&mut Shuffler>) -> Option<usize> {
        let at = self.current_index()?;
        if items.is_empty() {
            self.remove(at);
            return Some(at);
        }
        self.manual = true;
        let added = items.len() - 1;
        self.items.splice(at..=at, items);
        if let Some(s) = &mut self.shuffle {
            // 後面的項目往後移；第一個換進來的就是原本那一項的位置（目前的），其餘的是新的
            s.remap(|k| if k > at { k + added } else { k });
            let lo = s.pos + 1;
            s.place_unplayed(lo, (at + 1..=at + added).collect(), rng, true);
        }
        self.debug_check();
        Some(at)
    }

    /// 隨機播放時，從 `range`（目前的檔案和這一輪還沒播的幾項，例如 [`Self::replace_current_with`] 剛換進來的）
    /// 隨機選一項當成目前的檔案：它換到目前那一格，原本目前的那一項換到它原本的位置（變成還沒播）。
    /// 網路上的播放清單整份一起開時，第一個也是隨機的。回傳選到的項目；沒開隨機、目前的檔案不在範圍裡時 None
    pub fn pick_current_in(&mut self, range: std::ops::Range<usize>, rng: &mut Shuffler) -> Option<usize> {
        if self.current_removed || !range.contains(&self.index) || range.end > self.items.len() {
            return None;
        }
        let s = self.shuffle.as_mut()?;
        let k = range.start + rng.below(range.len());
        let slot = s.slot_of(k);
        // 這一輪已經播過的不選（換進來的項目一定排在還沒播的部分）
        if slot < s.pos {
            return None;
        }
        s.order.swap(s.pos, slot);
        self.sync_index();
        self.debug_check();
        Some(k)
    }

    /// 依檔名自然排序，目前的檔案維持是目前的
    pub fn sort(&mut self) {
        self.manual = true;
        // 記住原本的位置再排序：目前那一項（或「下一個」）是同一項，不是第一個同名的
        let mut tagged: Vec<(usize, PathBuf)> = std::mem::take(&mut self.items).into_iter().enumerate().collect();
        tagged.sort_by(|(_, a), (_, b)| natural_cmp(&file_name_of(a), &file_name_of(b)));
        match &mut self.shuffle {
            Some(s) => {
                let mut new_of = vec![0; tagged.len()];
                for (new, (old, _)) in tagged.iter().enumerate() {
                    new_of[*old] = new;
                }
                s.remap(|k| new_of[k]);
            }
            None => {
                if let Some(i) = tagged.iter().position(|(i, _)| *i == self.index) {
                    self.index = i;
                }
            }
        }
        self.items = tagged.into_iter().map(|(_, p)| p).collect();
        self.sync_index();
        self.debug_check();
    }
}

/// 隨機播放時「跳到」清單位置 `i`：把它從原本那一格拿出來，放在目前的檔案後面
/// （目前的已經移出清單時放在「下一個」那一格），然後移到那裡。
/// 這一輪播過的（含剛才在播的）留在它前面，還沒播的照原本的順序排在它後面
fn jump(s: &mut ShuffleOrder, i: usize, current_removed: bool) {
    let from = s.slot_of(i);
    if from >= s.order.len() {
        return;
    }
    s.order.remove(from);
    if from < s.pos {
        s.pos -= 1;
    }
    let slot = (if current_removed { s.pos } else { s.pos + 1 }).min(s.order.len());
    s.order.insert(slot, i);
    s.pos = slot;
}

/// 一個檔案自然播完時接下來要做的事（[`end_step`]）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndStep {
    /// 播清單上的這一項（之後用 [`Playlist::select_index`] 開它）
    Open(usize),
    /// 重複全部：從頭（[`Playlist::wrap_index`]，隨機時重新洗牌）
    Wrap,
    /// 重播同一個檔案（跳到開頭、繼續播）
    RestartSame,
    /// 資料夾還在掃描，掃完再決定
    WaitScan,
    /// 播放結束（「播完後」的動作從這裡開始）
    Finished,
}

/// 一個檔案自然播完時接下來做什麼（純函式，不碰播放器）。
/// - 重複單一檔案平常由 mpv 的 `loop-file` 重播，根本不會播完；`loop_file_applied` 是 false 時
///   （`VITASCOPE_MPV_OPTS` 自己指定了 `loop-file`，或設定失敗）才在這裡重播。
///   已經套用了還是播完（mpv 的「No media data to loop」），就照一般的規則往下走。
/// - 沒開「播完自動播放下一個」時就結束。
/// - 清單上有下一個就播下一個（隨機時照洗好的順序）；資料夾還在掃描時等掃完。
/// - 重複全部：有別的項目可以繞回去就從頭，只有目前這一個（或沒有清單）時重播它。
pub fn end_step(
    list: Option<&Playlist>,
    scanning: bool,
    auto_next: bool,
    repeat: Repeat,
    loop_file_applied: bool,
) -> EndStep {
    if repeat == Repeat::One && !loop_file_applied {
        return EndStep::RestartSame;
    }
    if !auto_next {
        return EndStep::Finished;
    }
    if let Some(i) = list.and_then(Playlist::next_index) {
        return EndStep::Open(i);
    }
    if scanning {
        return EndStep::WaitScan;
    }
    match repeat {
        Repeat::All if list.is_some_and(Playlist::can_wrap) => EndStep::Wrap,
        Repeat::All => EndStep::RestartSame,
        Repeat::Off | Repeat::One => EndStep::Finished,
    }
}

/// 自動接下一個（或重複全部繞回去）開的檔案開不起來（檔案不見了、網路磁碟斷線、不是影音檔…）：
/// 跳過它，接下來做什麼。清單上目前的項目就是開不起來的那一個。
/// - `failed` 是上次開成功之後開不起來的檔案（包括這一個）：清單上的每一項都在裡面（整輪都開不起來）就停，
///   不會一直繞。看的是哪些檔案、不是連續幾次：隨機播放重複全部時，新的一輪可能先試到已經失敗過的，
///   次數到了清單的長度也不代表每一項都試過（能播的那一個還沒輪到）。
/// - 開不起來的檔案不重播：重複單一檔案當成不重複；只剩它自己（[`EndStep::RestartSame`]）時也停。
/// - 不等資料夾掃描（自動接下一個開的一定是清單上的項目，清單已經完整了）。
pub fn skip_step(list: Option<&Playlist>, failed: &HashSet<PathBuf>, auto_next: bool, repeat: Repeat) -> EndStep {
    if list.is_none_or(|l| l.items().iter().all(|p| failed.contains(p))) {
        return EndStep::Finished;
    }
    let repeat = if repeat == Repeat::One { Repeat::Off } else { repeat };
    match end_step(list, false, auto_next, repeat, true) {
        EndStep::RestartSame | EndStep::WaitScan => EndStep::Finished,
        step => step,
    }
}

/// 隱藏檔：「.」開頭（包括 macOS 在外接磁碟留下的「._檔名」），以及 Windows 的隱藏 / 系統檔
fn is_hidden(e: &std::fs::DirEntry) -> bool {
    if e.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
        // Windows 上 DirEntry 的 metadata 來自目錄列表本身，不用另外查
        if e.metadata().is_ok_and(|m| m.file_attributes() & HIDDEN_OR_SYSTEM != 0) {
            return true;
        }
    }
    false
}

/// 比對用的鍵：跟 `same_file` 一樣（Windows 不分大小寫，網址一字不差），路徑分隔符號統一
fn file_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if crate::m3u::is_url(&s) {
        return s.into_owned();
    }
    let joined = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if cfg!(windows) { joined.to_lowercase() } else { joined }
}

/// 資料夾裡的影音檔，依檔名排序（隱藏檔、資料夾不算）。「加入資料夾」用
pub fn media_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| formats::media_kind(&e.path()).is_some())
                .filter(|e| !is_hidden(e))
                .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    sort_by_name(&mut items);
    items
}

/// 完整路徑（網址不動）。命令列、其他程式送來的相對路徑要在一開始就轉換：
/// 清單、播放紀錄、存起來的清單比對的都是完整路徑
pub fn absolute(path: &Path) -> PathBuf {
    if crate::m3u::is_url(&path.to_string_lossy()) {
        path.to_path_buf()
    } else {
        std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// 是否為同一個檔案。Windows 的檔名不分大小寫（包括中文以外的各種字母）。
/// 網址一字不差才算（YouTube 的影片代號分大小寫；`Path` 的比較還會把 `a//b` 當成 `a/b`）
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (sa, sb) = (a.to_string_lossy(), b.to_string_lossy());
    if crate::m3u::is_url(&sa) || crate::m3u::is_url(&sb) {
        return sa == sb;
    }
    if cfg!(windows) {
        a == b || a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// 依檔名的自然順序排序（第 2 集在第 10 集前面）
pub fn sort_by_name(items: &mut [PathBuf]) {
    items.sort_by(|a, b| natural_cmp(&file_name_of(a), &file_name_of(b)));
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 「自然排序」：數字部分照數值比，所以「第2集」排在「第10集」前面；其餘不分大小寫
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        digits.push(c);
                    }
                    digits
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m_trim, n_trim) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                // 位數多的比較大；位數一樣就逐字比（不會溢位，多長的數字都可以）
                let ord = m_trim.len().cmp(&n_trim.len()).then_with(|| m_trim.cmp(n_trim));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_for_episodes() {
        let mut names = vec![
            "第10集.mp4",
            "第2集.mp4",
            "第1集.mp4",
            "EP 03.mkv",
            "ep 1.mkv",
            "a.mp4",
            "A.mp4",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "A.mp4",
                "a.mp4",
                "ep 1.mkv",
                "EP 03.mkv",
                "第1集.mp4",
                "第2集.mp4",
                "第10集.mp4"
            ]
        );
        assert_eq!(
            natural_cmp("x007", "x7"),
            Ordering::Less,
            "數值相同時照原字串排，結果才穩定"
        );
        assert_eq!(natural_cmp("s01e9", "s01e10"), Ordering::Less);
        assert_eq!(
            natural_cmp("99999999999999999999999", "100000000000000000000000"),
            Ordering::Less,
            "很長的數字也不會溢位"
        );
    }

    #[test]
    fn steps_through_files() {
        let mut list = Playlist::from_files(vec!["a.mp4".into(), "b.mp4".into(), "c.mp4".into()]);
        assert_eq!(list.prev(), None);
        assert_eq!(list.next(), Some(Path::new("b.mp4")));
        assert!(list.select(Path::new("c.mp4")));
        assert_eq!(list.position(), 3);
        assert_eq!(list.next(), None);
        assert_eq!(list.prev(), Some(Path::new("b.mp4")));
        assert!(!list.select(Path::new("d.mp4")));
        assert_eq!(list.current(), Some(Path::new("c.mp4")), "選不到時維持原本的位置");
    }

    fn names(list: &Playlist) -> Vec<String> {
        list.items().iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn removing_items_keeps_the_current_file() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        list.select(Path::new("c"));
        list.remove(0);
        assert_eq!(list.current(), Some(Path::new("c")));
        assert_eq!(list.position(), 2);
        // 移掉正在播的：繼續播，下一個是原本接在後面的 d，上一個是 b
        list.remove(1);
        assert_eq!(names(&list), ["b", "d"]);
        assert_eq!(list.current(), None);
        assert_eq!(list.current_index(), None);
        assert_eq!(list.next(), Some(Path::new("d")));
        assert_eq!(list.prev(), Some(Path::new("b")));
        list.remove(9);
        assert_eq!(list.len(), 2, "超出範圍的不理會");
    }

    #[test]
    fn moving_items_follows_the_current_file() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        list.select(Path::new("b"));
        // 把 d 拖到最前面
        list.move_item(3, 0);
        assert_eq!(names(&list), ["d", "a", "b", "c"]);
        assert_eq!(list.current(), Some(Path::new("b")));
        // 把 a 拖到最後（to = len）
        list.move_item(1, 4);
        assert_eq!(names(&list), ["d", "b", "c", "a"]);
        assert_eq!(list.current_index(), Some(1));
        // 放回原位：不動
        list.move_item(2, 3);
        assert_eq!(names(&list), ["d", "b", "c", "a"]);
        // 正在播的已移出清單時，「下一個」跟著原本那一項走
        list.remove(1);
        assert_eq!(list.next(), Some(Path::new("c")));
        list.move_item(2, 0);
        assert_eq!(names(&list), ["a", "d", "c"]);
        assert_eq!(list.next(), Some(Path::new("c")));
        list.move_item(2, 0);
        assert_eq!(names(&list), ["c", "a", "d"]);
        assert_eq!(list.next(), Some(Path::new("c")), "拖的就是下一個");
        assert_eq!(list.prev(), None);
    }

    #[test]
    fn extend_skips_duplicates_and_sort_keeps_current() {
        let mut list = Playlist::from_files(vec!["第10集".into(), "第2集".into()]);
        list.select(Path::new("第2集"));
        assert_eq!(list.extend(["第1集".into(), "第2集".into()]), 1);
        list.sort();
        assert_eq!(names(&list), ["第1集", "第2集", "第10集"]);
        assert_eq!(list.current(), Some(Path::new("第2集")));
        // 正在播的已移出清單：排序後「下一個」還是同一個檔案
        list.remove(1);
        assert_eq!(list.next(), Some(Path::new("第10集")));
        list.extend(["第0集".into()]);
        list.sort();
        assert_eq!(list.next(), Some(Path::new("第10集")));
    }

    #[test]
    fn remote_playlist_replaces_its_own_entry() {
        // 本機的清單裡第 2 項是網路上的播放清單：展開成裡面的項目，前後不動
        let mut list = Playlist::from_files(vec!["a".into(), "http://h/list.m3u".into(), "c".into()]);
        assert!(list.select_index(1));
        let at = list.replace_current(vec!["http://h/1.mp4".into(), "http://h/2.mp4".into()]);
        assert_eq!(at, Some(1));
        assert_eq!(names(&list), ["a", "http://h/1.mp4", "http://h/2.mp4", "c"]);
        assert!(list.is_manual());
        assert_eq!(list.current(), Some(Path::new("http://h/1.mp4")));
        // 沒有目前的項目：不動
        let mut restored = Playlist::restored(vec!["a".into()], Some(0));
        assert_eq!(restored.replace_current(vec!["x".into()]), None);
        assert_eq!(names(&restored), ["a"]);
        // 展開後是空的
        let mut one = Playlist::from_files(vec!["http://h/list.m3u".into()]);
        one.select_index(0);
        assert_eq!(one.replace_current(Vec::new()), Some(0));
        assert!(one.is_empty() && one.current().is_none());
    }

    #[test]
    fn edits_make_a_list_manual_and_restored_lists_start_before_the_saved_item() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into()]);
        assert!(!list.is_manual());
        list.sort();
        assert!(list.is_manual());
        let restored = Playlist::restored(vec!["a".into(), "b".into(), "c".into()], Some(1));
        assert_eq!(restored.current(), None, "還沒開始播");
        assert_eq!(restored.next(), Some(Path::new("b")), "PgDn 從上次那一項開始");
        assert!(restored.is_manual());
    }

    #[test]
    fn duplicates_prefer_the_current_entry() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "a".into()]);
        list.index = 2;
        assert!(list.select(Path::new("a")));
        assert_eq!(list.position(), 3, "已經在第二個 a 上，不會跳回第一個");
    }

    #[test]
    fn clearing_then_adding_starts_with_the_first_added_file() {
        let mut list = Playlist::cleared();
        list.extend(["x".into(), "y".into()]);
        assert_eq!(list.current(), None, "還沒有在播的項目");
        assert_eq!(list.next(), Some(Path::new("x")));
        assert_eq!(list.resume_index(), Some(0));
    }

    #[test]
    fn moving_and_sorting_keep_the_current_copy_of_a_duplicate() {
        // a, b, a, c：正在播第二個 a
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "a".into(), "c".into()]);
        list.select_index(2);
        // 把 c 拖到最前面：c, a, b, a → 正在播的是最後一個 a
        list.move_item(3, 0);
        assert_eq!(list.position(), 4);
        assert_eq!(list.next(), None, "第二個 a 是最後一項");
        // 依檔名排序：a, a, b, c → 正在播的是第二個 a（排序是穩定的）
        list.sort();
        assert_eq!(list.position(), 2);
        assert_eq!(list.next(), Some(Path::new("b")));
        // 移動別的項目到它前面
        list.move_item(3, 0); // c, a, a, b
        assert_eq!(list.position(), 3);
        assert_eq!(list.current(), Some(Path::new("a")));
    }

    #[test]
    fn duplicates_are_reached_by_index() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "a".into(), "c".into()]);
        list.select_index(1);
        assert_eq!(list.next_index(), Some(2));
        list.select_index(2);
        assert_eq!(list.next(), Some(Path::new("c")), "第二個 a 之後是 c，不會繞回 b");
        assert_eq!(list.prev_index(), Some(1));
    }

    #[test]
    fn extend_dedupes_like_same_file() {
        let mut list = Playlist::from_files(vec!["dir/A.mp4".into()]);
        let added = list.extend(["dir/a.mp4".into(), "dir/b.mp4".into(), "dir/b.mp4".into()]);
        if cfg!(windows) {
            assert_eq!(added, 1, "Windows 不分大小寫");
        } else {
            assert_eq!(added, 2);
        }
        // 很多檔案也很快
        let many: Vec<PathBuf> = (0..20_000).map(|i| PathBuf::from(format!("dir/{i}.mp4"))).collect();
        let start = std::time::Instant::now();
        list.extend(many);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn urls_compare_exactly() {
        let same = |a: &str, b: &str| same_file(Path::new(a), Path::new(b));
        assert!(same("https://youtu.be/AbC", "https://youtu.be/AbC"));
        // 影片代號分大小寫（Windows 也是）
        assert!(!same("https://youtu.be/AbC", "https://youtu.be/abc"));
        // Path 的比較會把這些當成一樣
        assert!(!same("https://x.example/a//b", "https://x.example/a/b"));
        assert!(!same("https://x.example/a/", "https://x.example/a"));
        assert!(!same("https://x.example/a/b", r"https://x.example\a\b"));
        // 本機路徑照舊
        assert!(same("dir/a.mp4", "dir/a.mp4"));
        assert_eq!(same("dir/A.mp4", "dir/a.mp4"), cfg!(windows));
        // 清單去重複也一樣
        let mut list = Playlist::from_files(vec!["https://youtu.be/AbC".into()]);
        let added = list.extend([
            "https://youtu.be/abc".into(),
            "https://youtu.be/AbC".into(),
            "https://x.example/a//b".into(),
            "https://x.example/a/b".into(),
        ]);
        assert_eq!(added, 3);
        assert!(list.contains(Path::new("https://youtu.be/abc")));
    }

    #[test]
    fn same_folder_same_kind() {
        let dir = std::env::temp_dir().join(format!("vitascope-playlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "第10集.mkv",
            "第2集.mp4",
            "第1集.mkv",
            "第1集.ass",
            "主題曲.mp3",
            "說明.txt",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let list = Playlist::for_file(&dir.join("第2集.mp4"));
        let names: Vec<String> = list
            .items
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["第1集.mkv", "第2集.mp4", "第10集.mkv"],
            "只收影片，字幕、音樂、文字檔不算"
        );
        assert_eq!(list.position(), 2);

        let music = Playlist::for_file(&dir.join("主題曲.mp3"));
        assert_eq!(music.len(), 1);

        // 不認得的副檔名：清單只有它自己
        let other = Playlist::for_file(&dir.join("說明.txt"));
        assert_eq!(other.len(), 1);
        assert_eq!(other.current(), Some(dir.join("說明.txt").as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ───────────── 播放順序（隨機、重複） ─────────────

    /// `n` 個項目：「0」「1」「2」…（路徑就是位置，比對方便）
    fn numbered(n: usize) -> Playlist {
        Playlist::from_files((0..n).map(|i| PathBuf::from(i.to_string())).collect())
    }

    fn order_of(list: &Playlist) -> (Vec<usize>, usize) {
        let (order, pos) = list.shuffle_order().expect("開著隨機播放");
        (order.to_vec(), pos)
    }

    /// 順序換成路徑（清單位置會因為拖曳、排序而變，路徑不會）
    fn order_paths(list: &Playlist) -> Vec<String> {
        let (order, _) = order_of(list);
        order
            .iter()
            .map(|&k| list.items()[k].to_string_lossy().into_owned())
            .collect()
    }

    /// 一直按「下一個」（跟 app 一樣：next_index 再 select_index）直到沒有下一個，回傳經過的清單位置
    fn walk_to_end(list: &mut Playlist) -> Vec<usize> {
        let mut seen = Vec::new();
        while let Some(i) = list.next_index() {
            assert!(list.select_index(i));
            seen.push(i);
            assert!(seen.len() <= list.len(), "一輪播不完：{list:?}");
        }
        seen
    }

    /// 往下一個走 `n` 步，回傳經過的清單位置
    fn step_forward(list: &mut Playlist, n: usize) -> Vec<usize> {
        (0..n)
            .map(|_| {
                let i = list.next_index().expect("還有下一個");
                assert!(list.select_index(i));
                i
            })
            .collect()
    }

    #[test]
    fn shuffler_is_deterministic_with_seed() {
        let take = |seed| {
            let mut r = Shuffler::seeded(seed);
            (0..8).map(|_| r.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(take(42), take(42), "同一個種子，同一串數字");
        assert_ne!(take(42), take(43));
        // 0 跟 1 都只有一個答案；其他的落在範圍內，而且大致平均
        let mut r = Shuffler::seeded(7);
        assert_eq!(r.below(0), 0);
        assert_eq!(r.below(1), 0);
        let mut counts = [0usize; 3];
        for _ in 0..30_000 {
            counts[r.below(3)] += 1;
        }
        assert!(counts.iter().all(|&c| (9_000..11_000).contains(&c)), "{counts:?}");
        // 同一個種子洗出同一個順序
        let shuffled = |seed| {
            let mut list = numbered(10);
            list.set_shuffle(true, &mut Shuffler::seeded(seed));
            order_of(&list).0
        };
        assert_eq!(shuffled(1), shuffled(1));
        let distinct: HashSet<Vec<usize>> = (0..50).map(shuffled).collect();
        assert!(distinct.len() > 40, "不同的種子洗出不同的順序：{}", distinct.len());
    }

    #[test]
    fn shuffle_starts_with_current() {
        let mut list = numbered(5);
        list.select_index(2);
        list.set_shuffle(true, &mut Shuffler::seeded(1));
        assert!(list.is_shuffled());
        let (order, pos) = order_of(&list);
        assert_eq!((order[0], pos), (2, 0), "目前的檔案排第一個");
        assert_eq!(list.current_index(), Some(2), "目前的檔案不變");
        assert_eq!(list.prev_index(), None, "這一輪的開頭：沒有上一個");
        assert_eq!(list.order_position(), 1);
        assert_eq!(list.position(), 3, "清單上的位置照舊");
        // 已經開著時再打開，不重新洗牌（播過哪些照舊）
        step_forward(&mut list, 2);
        let before = order_of(&list);
        list.set_shuffle(true, &mut Shuffler::seeded(99));
        assert_eq!(order_of(&list), before);
        assert_eq!(list.order_position(), 3, "這一輪的第 3 個");
    }

    #[test]
    fn shuffle_visits_every_item_once() {
        let mut firsts = HashSet::new();
        for seed in 0..200 {
            let mut list = numbered(8);
            list.select_index(3);
            list.set_shuffle(true, &mut Shuffler::seeded(seed));
            let walked = walk_to_end(&mut list);
            let all: HashSet<usize> = walked.iter().copied().chain([3]).collect();
            assert_eq!(walked.len(), 7, "seed {seed}: {walked:?}");
            assert_eq!(all.len(), 8, "seed {seed}：每一項都播一次：{walked:?}");
            assert!(!walked.contains(&3), "開著的那一個不會再播一次");
            assert_eq!(list.next_index(), None);
            assert_eq!(list.order_position(), 8);
            firsts.insert(walked[0]);
        }
        assert_eq!(firsts.len(), 7, "下一個是隨機的：{firsts:?}");
    }

    #[test]
    fn prev_walks_back_through_history() {
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(5));
        let mut played = vec![0];
        played.extend(step_forward(&mut list, 4));
        // 不移動位置時，上一個就是前一個播的
        assert_eq!(list.prev_index(), Some(played[3]));
        assert_eq!(list.prev(), Some(list.items()[played[3]].as_path()));
    }

    /// S-A2：一步（PgUp / PgDn、自動接下一個）只移動位置，不是「跳」；交替問上一個、選它，要一路往回走
    #[test]
    fn alternating_prev_and_select_walks_back_through_history() {
        for seed in 0..50 {
            let mut list = numbered(7);
            list.set_shuffle(true, &mut Shuffler::seeded(seed));
            let mut played = vec![0];
            played.extend(step_forward(&mut list, 4));
            let order_before = order_of(&list).0;
            // PgUp 三次：played[3]、played[2]、played[1]
            for back in 1..=3 {
                let p = list.prev_index().expect("還有上一個");
                assert_eq!(p, played[4 - back], "seed {seed}：第 {back} 次 PgUp");
                assert!(list.select_index(p));
                assert_eq!(list.current_index(), Some(p));
            }
            assert_eq!(list.order_position(), 2);
            assert_eq!(order_of(&list).0, order_before, "seed {seed}：往回走不改順序");
            // 再往前（PgDn）照原本的順序重播
            assert_eq!(step_forward(&mut list, 3), played[2..=4].to_vec(), "seed {seed}");
            // 播過的不會被當成還沒播：剩下的就是原本還沒播的
            let rest = walk_to_end(&mut list);
            assert_eq!(rest, order_before[5..].to_vec(), "seed {seed}");
        }
    }

    #[test]
    fn jump_to_played_and_unplayed_items() {
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(3));
        step_forward(&mut list, 2);
        let (o, pos) = order_of(&list);
        assert_eq!(pos, 2);
        // 跳到還沒播、也不是下一個的 o[4]：放在目前的後面，原本的下一個 o[3] 接在它後面
        assert!(list.select_index(o[4]));
        assert_eq!(order_of(&list), (vec![o[0], o[1], o[2], o[4], o[3], o[5]], 3));
        assert_eq!((list.prev_index(), list.next_index()), (Some(o[2]), Some(o[3])));
        // 跳回播過的 o[0]（不是上一個）：剛才播的 o[4] 算播過，o[0] 排在它後面再播一次
        assert!(list.select_index(o[0]));
        assert_eq!(order_of(&list), (vec![o[1], o[2], o[4], o[0], o[3], o[5]], 3));
        assert_eq!((list.prev_index(), list.next_index()), (Some(o[4]), Some(o[3])));
        // 選目前的：不動
        assert!(list.select_index(o[0]));
        assert_eq!(order_of(&list), (vec![o[1], o[2], o[4], o[0], o[3], o[5]], 3));
        // 用路徑選（最近開啟的檔案、拖放）一律是「跳」：連上一個也是
        let prev_path = list.items()[o[4]].clone();
        assert!(list.select(&prev_path));
        assert_eq!(order_of(&list), (vec![o[1], o[2], o[0], o[4], o[3], o[5]], 3));
        // 用路徑選下一個，跟一步一樣
        let next_path = list.items()[o[3]].clone();
        assert!(list.select(&next_path));
        assert_eq!(order_of(&list), (vec![o[1], o[2], o[0], o[4], o[3], o[5]], 4));
        assert_eq!(walk_to_end(&mut list), vec![o[5]]);
    }

    #[test]
    fn remove_current_while_shuffled_keeps_next() {
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(11));
        step_forward(&mut list, 2);
        let paths = order_paths(&list);
        let current = list.current_index().unwrap();
        list.remove(current);
        assert_eq!(list.current(), None, "繼續播，但已經不在清單上");
        assert_eq!(list.next().unwrap().to_string_lossy(), paths[3], "下一個是原本的下一個");
        assert_eq!(list.prev().unwrap().to_string_lossy(), paths[1], "上一個是前一個播的");
        assert_eq!(list.resume_index(), list.next_index(), "存檔時記下一個");
        // 下一個（PgDn）是一步，不是跳
        let next = list.next_index().unwrap();
        assert!(list.select_index(next));
        assert_eq!(list.order_position(), 3);
        let mut expected = paths.clone();
        expected.remove(2);
        assert_eq!(order_paths(&list), expected);
        // 移掉的是順序的最後一個：沒有下一個
        let mut list = numbered(3);
        list.set_shuffle(true, &mut Shuffler::seeded(2));
        walk_to_end(&mut list);
        let last = list.current_index().unwrap();
        list.remove(last);
        assert_eq!((list.next_index(), list.resume_index()), (None, None));
        assert!(list.prev_index().is_some());
    }

    #[test]
    fn remove_other_items_keeps_order() {
        let mut list = numbered(7);
        list.set_shuffle(true, &mut Shuffler::seeded(4));
        step_forward(&mut list, 3);
        let mut paths = order_paths(&list);
        let current = list.current().unwrap().to_path_buf();
        // 移掉一個播過的、兩個還沒播的
        for slot in [1usize, 5, 4] {
            let (order, _) = order_of(&list);
            let target = paths.remove(slot);
            assert_eq!(list.items()[order[slot]].to_string_lossy(), target);
            list.remove(order[slot]);
            assert_eq!(order_paths(&list), paths);
            assert_eq!(list.current(), Some(current.as_path()), "目前的檔案不變");
        }
        assert_eq!(list.order_position(), 3, "移掉一個播過的，往前一格");
        assert_eq!(list.next().unwrap().to_string_lossy(), paths[3]);
    }

    #[test]
    fn removing_the_next_after_the_current_was_removed() {
        // S-B2：目前的已經移出清單時再移掉「下一個」，index 要重新從順序讀
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(8));
        step_forward(&mut list, 1);
        let paths = order_paths(&list);
        list.remove(list.current_index().unwrap());
        let next = list.next_index().unwrap();
        list.remove(next);
        assert_eq!(list.next().unwrap().to_string_lossy(), paths[3], "再下一個");
        assert_eq!(list.resume_index(), list.next_index());
        assert_eq!(list.prev().unwrap().to_string_lossy(), paths[0]);
        let rest: Vec<String> = walk_to_end(&mut list)
            .iter()
            .map(|&i| list.items()[i].to_string_lossy().into_owned())
            .collect();
        assert_eq!(rest, paths[3..].to_vec());
    }

    #[test]
    fn jump_after_the_current_was_removed_keeps_the_next_unplayed() {
        // 目前的已經移出清單時「跳」：選的那一項放在「下一個」那一格，原本的下一個排在它後面，這一輪照樣會播
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(5));
        step_forward(&mut list, 2);
        let paths = order_paths(&list);
        list.remove(list.current_index().unwrap());
        // 移掉後：順序是 paths[0], paths[1], paths[3], paths[4], paths[5]，pos 2 指著 paths[3]（下一個）
        let (o, pos) = order_of(&list);
        assert_eq!(pos, 2);
        assert_eq!(list.next_index(), Some(o[2]));
        // 跳到還沒播、也不是下一個的 o[3]
        assert!(list.select_index(o[3]));
        assert_eq!(order_of(&list), (vec![o[0], o[1], o[3], o[2], o[4]], 2));
        assert_eq!(list.order_position(), 3);
        assert_eq!((list.prev_index(), list.next_index()), (Some(o[1]), Some(o[2])));
        let rest: Vec<String> = walk_to_end(&mut list)
            .iter()
            .map(|&i| list.items()[i].to_string_lossy().into_owned())
            .collect();
        assert_eq!(rest, vec![paths[3].clone(), paths[5].clone()], "原本的下一個沒被跳過");

        // 跳到播過的（不是上一個）：一樣放在「下一個」那一格，原本的下一個接在它後面
        let mut list = numbered(6);
        list.set_shuffle(true, &mut Shuffler::seeded(9));
        step_forward(&mut list, 3);
        list.remove(list.current_index().unwrap());
        let (o, pos) = order_of(&list);
        assert_eq!(pos, 3);
        assert!(list.select_index(o[0]));
        assert_eq!(order_of(&list), (vec![o[1], o[2], o[0], o[3], o[4]], 2));
        assert_eq!(list.next_index(), Some(o[3]));

        // 還原上次的清單、開著隨機播放，雙擊別的檔案：上次那一項還是下一個
        for seed in 0..50 {
            let mut list = Playlist::restored((0..5).map(|i| PathBuf::from(i.to_string())).collect(), Some(3));
            list.set_shuffle(true, &mut Shuffler::seeded(seed));
            assert!(list.select(Path::new("1")));
            assert_eq!(list.current_index(), Some(1));
            assert_eq!(list.order_position(), 1);
            assert_eq!(list.next_index(), Some(3), "上次播的那一項這一輪照樣會播");
            assert_eq!(walk_to_end(&mut list).len(), 4);
        }
    }

    #[test]
    fn extend_while_shuffled_puts_new_items_in_unplayed_part() {
        let mut next_is_new = 0;
        for seed in 0..200 {
            let mut list = numbered(5);
            let mut rng = Shuffler::seeded(seed);
            list.set_shuffle(true, &mut rng);
            step_forward(&mut list, 2);
            let before = order_paths(&list);
            assert_eq!(
                list.extend_with(["a".into(), "b".into(), "c".into(), "0".into()], &mut rng),
                3
            );
            let after = order_paths(&list);
            assert_eq!(after[..3], before[..3], "seed {seed}：播過的和目前的不動");
            assert_eq!(list.order_position(), 3);
            let old_unplayed: Vec<&String> = after[3..].iter().filter(|p| before.contains(p)).collect();
            assert_eq!(
                old_unplayed,
                before[3..].iter().collect::<Vec<_>>(),
                "原本還沒播的順序不變"
            );
            next_is_new += usize::from(["a", "b", "c"].contains(&after[3].as_str()));
        }
        assert!(
            (50..190).contains(&next_is_new),
            "新加的打散在還沒播的部分：{next_is_new}"
        );
        // 沒有亂數（extend）時照順序放在最後
        let mut list = numbered(3);
        list.set_shuffle(true, &mut Shuffler::seeded(1));
        let before = order_paths(&list);
        list.extend(["x".into(), "y".into()]);
        assert_eq!(order_paths(&list), [before, vec!["x".into(), "y".into()]].concat());
    }

    #[test]
    fn extend_after_clear_while_shuffled() {
        let mut rng = Shuffler::seeded(6);
        let mut list = Playlist::cleared();
        list.set_shuffle(true, &mut rng);
        assert_eq!(order_of(&list), (vec![], 0));
        assert_eq!(list.extend_with(["x".into(), "y".into(), "z".into()], &mut rng), 3);
        assert_eq!(list.current(), None, "還沒有在播的項目");
        let first = list.next_index().expect("加進來的就是下一個");
        assert_eq!(list.resume_index(), Some(first));
        let mut walked = walk_to_end(&mut list);
        assert_eq!(walked[0], first);
        walked.sort_unstable();
        assert_eq!(walked, [0, 1, 2], "每一項播一次");
        // 沒有亂數也一樣
        let mut list = Playlist::cleared();
        list.set_shuffle(true, &mut rng);
        list.extend(["x".into(), "y".into()]);
        assert_eq!(list.next(), Some(Path::new("x")));
        // 空的清單（沒有標成「還沒開始」）也不會溢位
        let mut list = Playlist::from_files(Vec::new());
        list.set_shuffle(true, &mut rng);
        list.extend_with(["x".into(), "y".into()], &mut rng);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn extend_after_removing_current_while_shuffled() {
        let mut new_is_next = 0;
        for seed in 0..200 {
            let mut rng = Shuffler::seeded(seed);
            let mut list = numbered(4);
            list.set_shuffle(true, &mut rng);
            step_forward(&mut list, 1);
            let before = order_paths(&list);
            list.remove(list.current_index().unwrap());
            list.extend_with(["new".into()], &mut rng);
            let after = order_paths(&list);
            assert_eq!(after[0], before[0], "seed {seed}：播過的不動");
            // 還沒播的部分從 pos 開始：新加的可以排在 pos，成為下一個
            let next = list.next().unwrap().to_string_lossy().into_owned();
            assert_eq!(next, after[1], "seed {seed}");
            new_is_next += usize::from(next == "new");
            assert_eq!(walk_to_end(&mut list).len(), 3, "seed {seed}");
        }
        assert!((30..120).contains(&new_is_next), "{new_is_next}");
        // 移掉的是順序的最後一個（沒有下一個）：加進來的就是下一個
        let mut rng = Shuffler::seeded(1);
        let mut list = numbered(2);
        list.set_shuffle(true, &mut rng);
        walk_to_end(&mut list);
        list.remove(list.current_index().unwrap());
        assert_eq!(list.next_index(), None);
        list.extend_with(["new".into()], &mut rng);
        assert_eq!(list.next(), Some(Path::new("new")));
    }

    #[test]
    fn move_and_sort_keep_the_shuffle_identity() {
        let mut list = Playlist::from_files(
            ["第3集", "第1集", "第10集", "第2集", "第5集"]
                .map(PathBuf::from)
                .to_vec(),
        );
        list.set_shuffle(true, &mut Shuffler::seeded(9));
        step_forward(&mut list, 2);
        let paths = order_paths(&list);
        let current = list.current().unwrap().to_path_buf();
        for (from, to) in [(4, 0), (0, 5), (2, 1), (1, 4)] {
            list.move_item(from, to);
            assert_eq!(order_paths(&list), paths, "拖曳 {from} → {to}");
            assert_eq!(list.current(), Some(current.as_path()));
            assert_eq!(list.order_position(), 3);
        }
        list.sort();
        assert_eq!(names(&list), ["第1集", "第2集", "第3集", "第5集", "第10集"]);
        assert_eq!(order_paths(&list), paths, "排序後順序照舊");
        assert_eq!(list.current(), Some(current.as_path()));
        // 目前的已經移出清單時，「下一個」也跟著項目走
        list.remove(list.current_index().unwrap());
        let next = list.next().unwrap().to_path_buf();
        list.move_item(list.next_index().unwrap(), 0);
        assert_eq!(list.next(), Some(next.as_path()));
        list.sort();
        assert_eq!(list.next(), Some(next.as_path()));
    }

    #[test]
    fn move_item_without_shuffle_matches_the_old_rule() {
        // 改寫成共用的位置換算之前的規則：拖曳時目前那一項跟著項目走
        let old_rule = |index: usize, from: usize, to: usize| {
            let to = if to > from { to - 1 } else { to };
            if from == index {
                return to;
            }
            let mut index = index;
            if from < index {
                index -= 1;
            }
            if to <= index {
                index += 1;
            }
            index
        };
        for index in 0..=5 {
            for from in 0..5 {
                for to in 0..=5 {
                    if from == to || from + 1 == to {
                        continue;
                    }
                    let mut list = numbered(5);
                    list.index = index;
                    list.current_removed = index == 5;
                    list.move_item(from, to);
                    assert_eq!(
                        list.index,
                        old_rule(index, from, to),
                        "index {index} from {from} to {to}"
                    );
                }
            }
        }
    }

    #[test]
    fn wrap_reshuffles_and_does_not_repeat_the_last_item() {
        let mut changed = 0;
        for seed in 0..1000 {
            let mut rng = Shuffler::seeded(seed);
            let mut list = numbered(5);
            list.set_shuffle(true, &mut rng);
            walk_to_end(&mut list);
            let old = order_of(&list).0;
            let finished = list.current_index().unwrap();
            assert!(list.can_wrap());
            let first = list.wrap_index(&mut rng).expect("有別的項目");
            assert_ne!(first, finished, "seed {seed}：剛播完的不會馬上又播");
            let (order, pos) = order_of(&list);
            assert_eq!((order[0], pos), (finished, 0), "seed {seed}：剛播完的算這一輪播過了");
            assert_eq!(list.current_index(), Some(finished));
            assert!(list.select_index(first));
            assert_eq!(list.order_position(), 2, "seed {seed}：一步，不是跳");
            let mut cycle = vec![finished, first];
            cycle.extend(walk_to_end(&mut list));
            let mut sorted = cycle.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3, 4], "seed {seed}：新的一輪每一項播一次");
            // 剛播完的是上一輪的最後一個；其餘的順序重新洗過
            assert_eq!(old[4], finished);
            changed += usize::from(cycle[1..] != old[..4]);
        }
        assert!(changed > 900, "重新洗牌：{changed}");
        // 照清單順序：從第一項開始
        let mut rng = Shuffler::seeded(0);
        let mut list = numbered(3);
        list.select_index(2);
        assert_eq!(list.wrap_index(&mut rng), Some(0));
        assert_eq!(list.first_index(), Some(0));
        assert_eq!(list.last_index(), Some(2));
        // 只有目前這一個、或是空的：沒有可以繞回去的（重播目前的檔案）
        let mut one = numbered(1);
        assert!(!one.can_wrap());
        assert_eq!(one.wrap_index(&mut rng), None);
        assert_eq!((one.first_index(), one.last_index()), (None, None));
        one.set_shuffle(true, &mut rng);
        assert_eq!(one.wrap_index(&mut rng), None);
        assert_eq!(Playlist::cleared().wrap_index(&mut rng), None);
        // 只剩一項、但不是目前的（目前的已經移出清單）：繞回那一項
        let mut list = numbered(2);
        list.select_index(1);
        list.remove(1);
        assert!(list.can_wrap());
        assert_eq!(list.wrap_index(&mut rng), Some(0));
        list.set_shuffle(true, &mut rng);
        assert_eq!(list.wrap_index(&mut rng), Some(0));
        assert!(list.select_index(0));
        assert_eq!(list.order_position(), 1);
        // 隨機播放時沒有手動繞回去的目標
        let mut list = numbered(3);
        list.set_shuffle(true, &mut rng);
        assert_eq!((list.first_index(), list.last_index()), (None, None));
    }

    #[test]
    fn restored_list_shuffled_starts_with_saved_item() {
        for seed in 0..50 {
            let mut list = Playlist::restored((0..5).map(|i| PathBuf::from(i.to_string())).collect(), Some(3));
            list.set_shuffle(true, &mut Shuffler::seeded(seed));
            assert_eq!(list.current(), None, "還沒開始播");
            assert_eq!(list.next_index(), Some(3), "PgDn、開啟時從上次那一項開始");
            assert_eq!(list.resume_index(), Some(3));
            assert!(list.select_index(3));
            assert_eq!(list.order_position(), 1);
            assert_eq!(walk_to_end(&mut list).len(), 4);
        }
    }

    #[test]
    fn whole_list_shuffle_starts_at_random() {
        let mut firsts = HashSet::new();
        for seed in 0..100 {
            let mut list = numbered(4);
            let first = list.shuffle_whole_list(&mut Shuffler::seeded(seed)).unwrap();
            firsts.insert(first);
            assert_eq!(list.current(), None, "還沒開始播");
            assert_eq!(list.next_index(), Some(first));
            assert!(list.select_index(first));
            assert_eq!(list.order_position(), 1, "第一個是一步，不是跳");
            assert_eq!(walk_to_end(&mut list).len(), 3);
        }
        assert_eq!(firsts.len(), 4, "第一個也是隨機的：{firsts:?}");
        assert_eq!(Playlist::cleared().shuffle_whole_list(&mut Shuffler::seeded(0)), None);
    }

    #[test]
    fn duplicates_while_shuffled() {
        // a, b, a, c：靠位置區分，兩個 a 各播一次
        let mut list = Playlist::from_files(["a", "b", "a", "c"].map(PathBuf::from).to_vec());
        list.select_index(2);
        list.set_shuffle(true, &mut Shuffler::seeded(13));
        assert_eq!(order_of(&list).0[0], 2);
        assert!(list.select(Path::new("a")), "已經在第二個 a 上");
        assert_eq!(list.current_index(), Some(2), "不會跳回第一個 a");
        let mut walked = walk_to_end(&mut list);
        walked.push(2);
        walked.sort_unstable();
        assert_eq!(walked, [0, 1, 2, 3]);
    }

    #[test]
    fn shuffle_off_returns_to_list_order() {
        let mut rng = Shuffler::seeded(21);
        let mut list = numbered(6);
        list.set_shuffle(true, &mut rng);
        step_forward(&mut list, 3);
        let current = list.current_index().unwrap();
        list.set_shuffle(false, &mut rng);
        assert!(!list.is_shuffled() && list.shuffle_order().is_none());
        assert_eq!(list.current_index(), Some(current), "目前的檔案不變");
        assert_eq!(
            list.next_index(),
            (current + 1 < 6).then_some(current + 1),
            "照清單順序接下去"
        );
        assert_eq!(list.prev_index(), current.checked_sub(1));
        assert_eq!(list.order_position(), list.position());
        // 目前的已經移出清單時關掉：下一個照舊
        list.set_shuffle(true, &mut rng);
        step_forward(&mut list, 1);
        list.remove(list.current_index().unwrap());
        let next = list.next_index();
        list.set_shuffle(false, &mut rng);
        assert_eq!(list.next_index(), next);
    }

    #[test]
    fn remote_playlist_replaces_its_own_entry_while_shuffled() {
        let mut rng = Shuffler::seeded(17);
        let mut list = Playlist::from_files(["a", "http://h/list.m3u", "c", "d"].map(PathBuf::from).to_vec());
        list.select_index(1);
        list.set_shuffle(true, &mut rng);
        step_forward(&mut list, 1);
        // 回到清單那一項（跳），再展開
        assert!(list.select(Path::new("http://h/list.m3u")));
        let played = order_paths(&list)[..2].to_vec();
        let at = list.replace_current(["http://h/1", "http://h/2", "http://h/3"].map(PathBuf::from).to_vec());
        assert_eq!(at, Some(1));
        assert_eq!(names(&list), ["a", "http://h/1", "http://h/2", "http://h/3", "c", "d"]);
        assert_eq!(list.current(), Some(Path::new("http://h/1")));
        let paths = order_paths(&list);
        assert_eq!(
            (paths[0].as_str(), paths[1].as_str()),
            (played[0].as_str(), "http://h/1")
        );
        assert_eq!(paths[2..4], ["http://h/2", "http://h/3"], "沒有亂數：照順序接在後面");
        // 有亂數：其餘的打散到還沒播的部分
        let mut list = Playlist::from_files(["L", "x"].map(PathBuf::from).to_vec());
        list.set_shuffle(true, &mut rng);
        let items: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("http://h/{i}"))).collect();
        assert_eq!(list.replace_current_with(items, &mut rng), Some(0));
        assert_eq!(list.current(), Some(Path::new("http://h/0")));
        assert_eq!(list.order_position(), 1);
        let rest = order_paths(&list)[1..].to_vec();
        let in_list_order: Vec<String> = (1..20).map(|i| format!("http://h/{i}")).collect();
        assert_ne!(
            rest.iter()
                .filter(|p| p.starts_with("http"))
                .cloned()
                .collect::<Vec<_>>(),
            in_list_order
        );
        assert_eq!(walk_to_end(&mut list).len(), 20);
        // 展開後是空的：跟移出清單一樣
        let mut list = Playlist::from_files(["a", "http://h/list.m3u", "c"].map(PathBuf::from).to_vec());
        list.set_shuffle(true, &mut rng);
        list.select_index(1);
        assert_eq!(list.replace_current(Vec::new()), Some(1));
        assert_eq!(names(&list), ["a", "c"]);
        assert_eq!(list.current(), None);
    }

    #[test]
    fn random_edits_keep_the_shuffle_consistent() {
        // 隨機的一連串操作：每一步之後除錯版的檢查（debug_check）都要過，
        // 而且「一直按下一個」不會重播這一輪已經播過的項目
        for seed in 0..300 {
            let mut rng = Shuffler::seeded(seed);
            let mut ops = Shuffler::seeded(seed ^ 0xDEAD_BEEF);
            let mut list = numbered(1 + ops.below(6));
            list.set_shuffle(true, &mut rng);
            let mut fresh = 0;
            for _ in 0..60 {
                let len = list.len();
                match ops.below(14) {
                    0 | 1 => {
                        if let Some(i) = list.next_index() {
                            list.select_index(i);
                        }
                    }
                    2 => {
                        if let Some(i) = list.prev_index() {
                            list.select_index(i);
                        }
                    }
                    3 if len > 0 => {
                        list.select_index(ops.below(len));
                    }
                    4 if len > 0 => {
                        let p = list.items()[ops.below(len)].clone();
                        list.select(&p);
                    }
                    5 if len > 0 => list.remove(ops.below(len)),
                    6 if len > 0 => list.move_item(ops.below(len), ops.below(len + 1)),
                    7 => {
                        let n = ops.below(3) + 1;
                        let files: Vec<PathBuf> = (0..n).map(|k| PathBuf::from(format!("n{fresh}-{k}"))).collect();
                        list.extend_with(files, &mut rng);
                        fresh += 1;
                    }
                    8 => {
                        list.extend([PathBuf::from(format!("e{fresh}"))]);
                        fresh += 1;
                    }
                    9 => list.sort(),
                    10 => {
                        let files: Vec<PathBuf> = (0..ops.below(3))
                            .map(|k| PathBuf::from(format!("r{fresh}-{k}")))
                            .collect();
                        list.replace_current_with(files, &mut rng);
                        fresh += 1;
                    }
                    11 => {
                        list.wrap_index(&mut rng);
                    }
                    12 => {
                        list.set_shuffle(false, &mut rng);
                        list.set_shuffle(true, &mut rng);
                    }
                    13 if ops.below(4) == 0 => {
                        list.shuffle_whole_list(&mut rng);
                    }
                    _ => {}
                }
                list.debug_check();
            }
            let (order, pos) = order_of(&list);
            let unplayed: HashSet<usize> = order[pos..].iter().copied().collect();
            let walked = walk_to_end(&mut list);
            assert!(walked.iter().all(|i| unplayed.contains(i)), "seed {seed}");
            assert_eq!(walked.iter().collect::<HashSet<_>>().len(), walked.len(), "seed {seed}");
        }
    }

    #[test]
    fn end_step_matrix() {
        use EndStep::*;
        use Repeat::*;
        // 清單的各種狀態：(名稱, 清單, 可以繞回去)。下一個由清單自己算（隨機時照順序）
        let mut rng = Shuffler::seeded(1);
        let mut cases: Vec<(&str, Option<Playlist>, bool)> = Vec::new();
        for shuffle in [false, true] {
            // 先開隨機播放再操作，隨機時「在最後」才是走完一輪
            let make = |n: usize, walk: bool, remove_current: bool, rng: &mut Shuffler| {
                let mut l = if n == 0 { Playlist::cleared() } else { numbered(n) };
                l.set_shuffle(shuffle, rng);
                if walk {
                    walk_to_end(&mut l);
                }
                if remove_current {
                    l.remove(l.current_index().unwrap());
                }
                Some(l)
            };
            cases.push(("空的", make(0, false, false, &mut rng), false));
            cases.push(("一項", make(1, false, false, &mut rng), false));
            cases.push(("一項、目前的已移出", make(2, true, true, &mut rng), true));
            cases.push(("三項、在開頭", make(3, false, false, &mut rng), true));
            cases.push(("三項、在最後", make(3, true, false, &mut rng), true));
        }
        cases.push(("沒有清單", None, false));
        let mut checked = 0;
        for (name, list, can_wrap) in &cases {
            let next = list.as_ref().and_then(Playlist::next_index);
            assert_eq!(list.as_ref().is_some_and(Playlist::can_wrap), *can_wrap, "{name}");
            for auto_next in [false, true] {
                for repeat in [Off, One, All] {
                    for scanning in [false, true] {
                        for applied in [false, true] {
                            let got = end_step(list.as_ref(), scanning, auto_next, repeat, applied);
                            let want = if repeat == One && !applied {
                                RestartSame
                            } else if !auto_next {
                                Finished
                            } else if let Some(i) = next {
                                Open(i)
                            } else if scanning {
                                WaitScan
                            } else if repeat == All {
                                if *can_wrap { Wrap } else { RestartSame }
                            } else {
                                Finished
                            };
                            assert_eq!(
                                got, want,
                                "{name} auto_next={auto_next} {repeat:?} scanning={scanning} applied={applied}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 11 * 24);
        // 幾個代表性的情況寫死，不靠上面的規則
        let three = numbered(3);
        assert_eq!(end_step(Some(&three), false, true, Off, true), Open(1));
        let last = {
            let mut l = numbered(3);
            l.select_index(2);
            l
        };
        assert_eq!(end_step(Some(&last), false, true, Off, true), Finished);
        assert_eq!(end_step(Some(&last), true, true, Off, true), WaitScan, "資料夾還在掃描");
        assert_eq!(end_step(Some(&last), false, true, All, true), Wrap);
        assert_eq!(
            end_step(Some(&last), false, false, All, true),
            Finished,
            "沒開自動播下一個"
        );
        assert_eq!(
            end_step(Some(&last), false, true, One, false),
            RestartSame,
            "loop-file 沒套用"
        );
        assert_eq!(
            end_step(Some(&last), false, true, One, true),
            Finished,
            "No media data to loop"
        );
        assert_eq!(end_step(Some(&numbered(1)), false, true, All, true), RestartSame);
        assert_eq!(end_step(None, false, true, All, true), RestartSame);
        assert_eq!(end_step(None, false, true, Off, true), Finished);
    }

    #[test]
    fn pick_current_in_chooses_a_random_unplayed_spliced_item() {
        let urls = |n: usize| {
            (0..n)
                .map(|k| PathBuf::from(format!("http://h/{k}")))
                .collect::<Vec<_>>()
        };
        let mut seen = HashSet::new();
        for seed in 0..64 {
            let mut rng = Shuffler::seeded(seed);
            // 清單 0..4，播過 0、1，目前是 1；把 1 換成網路清單的 4 項（位置 1..5）
            let mut l = numbered(4);
            l.set_shuffle(true, &mut rng);
            let first = l.shuffle_order().unwrap().0[1];
            l.select_index(first);
            let played_before = l.shuffle_order().unwrap().0[0];
            let at = l.replace_current_with(urls(4), &mut rng).unwrap();
            assert_eq!(at, first);
            let k = l.pick_current_in(at..at + 4, &mut rng).unwrap();
            assert!((at..at + 4).contains(&k));
            seen.insert(k - at);
            let (order, pos) = l.shuffle_order().unwrap();
            assert_eq!((pos, order[pos]), (1, k), "選到的是目前的檔案，播過的還是只有一個");
            assert_eq!(order[0], played_before);
            assert_eq!(l.current_index(), Some(k));
            // 其他換進來的都在還沒播的部分
            for j in at..at + 4 {
                let slot = order.iter().position(|&e| e == j).unwrap();
                assert!(slot >= pos, "seed {seed}：{j} 在 {slot}");
            }
        }
        assert_eq!(seen.len(), 4, "每一項都可能是第一個：{seen:?}");
        // 沒開隨機、目前的已經移出清單、範圍不含目前的：不動
        let mut rng = Shuffler::seeded(1);
        let mut plain = numbered(3);
        assert_eq!(plain.pick_current_in(0..3, &mut rng), None);
        let mut s = numbered(3);
        s.set_shuffle(true, &mut rng);
        assert_eq!(s.pick_current_in(1..3, &mut rng), None);
        assert_eq!(s.pick_current_in(0..4, &mut rng), None, "超出清單");
        s.remove(0);
        assert_eq!(s.pick_current_in(0..2, &mut rng), None);
    }

    /// 上次開成功之後開不起來的檔案（清單上第 `idx` 項的路徑）
    fn failed_set(l: &Playlist, idx: &[usize]) -> HashSet<PathBuf> {
        idx.iter().map(|&i| l.items()[i].clone()).collect()
    }

    #[test]
    fn skip_step_moves_on_and_stops_after_a_whole_cycle_of_failures() {
        use EndStep::*;
        use Repeat::*;
        // 第 2 項（目前的）開不起來：跳到第 3 項；重複單一檔案不重播開不起來的
        let mut l = numbered(4);
        l.select_index(1);
        for repeat in [Off, One, All] {
            assert_eq!(
                skip_step(Some(&l), &failed_set(&l, &[1]), true, repeat),
                Open(2),
                "{repeat:?}"
            );
        }
        // 最後一項開不起來：不重複就結束，重複全部繞回去
        l.select_index(3);
        let f = failed_set(&l, &[3]);
        assert_eq!(skip_step(Some(&l), &f, true, Off), Finished);
        assert_eq!(skip_step(Some(&l), &f, true, One), Finished, "單一檔案不重播開不起來的");
        assert_eq!(skip_step(Some(&l), &f, true, All), Wrap);
        // 還有沒失敗過的：繼續繞；清單上的每一項都開不起來：停，不再繞
        assert_eq!(skip_step(Some(&l), &failed_set(&l, &[1, 2, 3]), true, All), Wrap);
        assert_eq!(skip_step(Some(&l), &failed_set(&l, &[0, 1, 2, 3]), true, All), Finished);
        l.select_index(1);
        assert_eq!(
            skip_step(Some(&l), &failed_set(&l, &[0, 1, 2, 3]), true, Off),
            Finished,
            "還有下一個也停"
        );
        // 失敗過好幾次的同一個檔案只算一個（次數不算數）
        let mut dup = Playlist::from_files(["a", "b", "b", "c"].map(PathBuf::from).to_vec());
        dup.select_index(2);
        assert_eq!(skip_step(Some(&dup), &failed_set(&dup, &[1, 2]), true, All), Open(3));
        // 每個檔案都開不起來了（重複的那一個失敗一次就算）：停，不是等到失敗次數到清單的長度
        assert_eq!(
            skip_step(Some(&dup), &failed_set(&dup, &[0, 1, 3]), true, All),
            Finished
        );
        // 失敗過、之後從清單拿掉的檔案不算：清單上還有沒試過的（第 1 項）就繼續
        let mut gone = failed_set(&l, &[1, 2, 3]);
        gone.insert(PathBuf::from("removed.mp4"));
        assert_eq!(skip_step(Some(&l), &gone, true, All), Open(2));
        // 中途關掉了自動播放下一個
        assert_eq!(skip_step(Some(&l), &failed_set(&l, &[1]), false, All), Finished);
        // 只有它自己：不重播
        let one = numbered(1);
        assert_eq!(skip_step(Some(&one), &failed_set(&one, &[0]), true, All), Finished);
        assert_eq!(skip_step(None, &HashSet::new(), true, All), Finished);
        // 隨機播放：照洗好的順序跳到下一個
        let mut rng = Shuffler::seeded(9);
        let mut s = numbered(5);
        s.set_shuffle(true, &mut rng);
        let next = s.next_index().unwrap();
        s.select_index(next);
        assert_eq!(
            skip_step(Some(&s), &failed_set(&s, &[next]), true, Off),
            Open(s.next_index().unwrap())
        );
    }

    /// 照播放器的做法模擬：目前的檔案播完（[`end_step`]），之後一直試開，開不起來就 [`skip_step`]。
    /// 回傳試開了哪些項目，和開成功的那一個（播放結束時 None）
    fn after_end(
        l: &mut Playlist,
        rng: &mut Shuffler,
        repeat: Repeat,
        playable: impl Fn(usize) -> bool,
    ) -> (Vec<usize>, Option<usize>) {
        let mut tried = Vec::new();
        let mut failed = HashSet::new();
        let mut step = end_step(Some(l), false, true, repeat, true);
        loop {
            let i = match step {
                EndStep::Open(i) => {
                    l.select_index(i);
                    i
                }
                EndStep::Wrap => {
                    let i = l.wrap_index(rng).unwrap();
                    l.select_index(i);
                    i
                }
                _ => return (tried, None),
            };
            tried.push(i);
            assert!(tried.len() <= 3 * l.len(), "一直繞：{tried:?}");
            if playable(i) {
                return (tried, Some(i));
            }
            failed.insert(l.items()[i].clone());
            step = skip_step(Some(l), &failed, true, repeat);
        }
    }

    #[test]
    fn shuffled_repeat_all_keeps_skipping_until_the_playable_file_comes_round() {
        // 三個裡只有 0 能開：隨機播放、重複全部時，新的一輪可能先試到已經失敗過的，照樣要繞到 0，不能停
        for seed in 0..200 {
            let mut rng = Shuffler::seeded(seed);
            let mut l = numbered(3);
            l.set_shuffle(true, &mut rng);
            assert_eq!(l.current_index(), Some(0));
            for round in 0..4 {
                let (tried, ok) = after_end(&mut l, &mut rng, Repeat::All, |i| i == 0);
                assert_eq!(ok, Some(0), "seed {seed} 第 {round} 輪：試了 {tried:?}");
            }
        }
        // 照清單順序也一樣
        let mut rng = Shuffler::seeded(1);
        let mut l = numbered(3);
        let (tried, ok) = after_end(&mut l, &mut rng, Repeat::All, |i| i == 0);
        assert_eq!((tried, ok), (vec![1, 2, 0], Some(0)));
        // 全部都開不起來（能播的那一個也斷線了）：每一項都試過就停，不會一直繞
        for seed in 0..50 {
            let mut rng = Shuffler::seeded(seed);
            let mut l = numbered(4);
            l.set_shuffle(true, &mut rng);
            let (tried, ok) = after_end(&mut l, &mut rng, Repeat::All, |_| false);
            assert_eq!(ok, None);
            let distinct: HashSet<usize> = tried.iter().copied().collect();
            assert_eq!(distinct.len(), 4, "seed {seed}：每一項都試過才停：{tried:?}");
        }
    }

    #[test]
    fn play_order_serialises_in_kebab_case_with_defaults() {
        assert_eq!(
            PlayOrder::default(),
            PlayOrder {
                shuffle: false,
                repeat: Repeat::Off
            }
        );
        let all = PlayOrder {
            shuffle: true,
            repeat: Repeat::All,
        };
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(json, r#"{"shuffle":true,"repeat":"all"}"#);
        assert_eq!(serde_json::from_str::<PlayOrder>(&json).unwrap(), all);
        assert_eq!(serde_json::to_string(&Repeat::One).unwrap(), r#""one""#);
        assert_eq!(serde_json::to_string(&Repeat::Off).unwrap(), r#""off""#);
        // 少了欄位用預設值
        assert_eq!(
            serde_json::from_str::<PlayOrder>(r#"{"repeat":"one"}"#).unwrap(),
            PlayOrder {
                shuffle: false,
                repeat: Repeat::One
            }
        );
        assert_eq!(serde_json::from_str::<PlayOrder>("{}").unwrap(), PlayOrder::default());
    }
}
