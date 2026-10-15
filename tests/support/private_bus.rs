//! Linux：測試自己開的 D-Bus 匯流排（`dbus-daemon`，用完就關），絕不是使用者的工作階段匯流排。
//!
//! 用自己的最小設定、不用系統的 session.conf：那裡面有標準的服務目錄，呼叫沒人提供的名稱時匯流排會去啟動
//! 這台電腦上真的服務（螢幕保護程式、桌面的設定 portal…）。
//! 沒有 dbus-daemon 時：開發機上跳過並說明；CI（有 `CI` 變數）上失敗，不能悄悄跳過（ci.yml 的 apt 要裝 dbus-daemon）。

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 測試自己的匯流排（Drop 時關掉）
pub struct PrivateBus {
    child: Child,
    /// 固定的 socket 路徑、不帶 guid：[`PrivateBus::restart`] 之後同一個位址還連得上
    pub address: String,
    dir: PathBuf,
    conf: PathBuf,
}

impl PrivateBus {
    /// `what`：哪些測試要它（跳過時的說明）。None = 這台電腦沒有 dbus-daemon（不在 CI 上：跳過）
    pub fn start(what: &str) -> Option<Self> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vitascope-test-bus-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.join("bus").display()
            ),
        )
        .unwrap();
        let child = match spawn(&conf) {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI 上一定要有 dbus-daemon（ci.yml 的 apt 要裝 dbus-daemon）：{what}的測試不能跳過"
                );
                eprintln!("跳過：找不到 dbus-daemon（apt install dbus-daemon），{what}的測試沒有跑");
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
            Err(e) => panic!("無法執行 dbus-daemon：{e}"),
        };
        let address = format!("unix:path={}", dir.join("bus").display());
        Some(Self {
            child,
            address,
            dir,
            conf,
        })
    }

    /// 匯流排重新開起來（像重新登入）：同一個位址，之前連著的都斷了、上面什麼都沒有
    pub fn restart(&mut self) {
        self.kill();
        let _ = std::fs::remove_file(self.dir.join("bus"));
        self.child = spawn(&self.conf).unwrap_or_else(|e| panic!("無法執行 dbus-daemon：{e}"));
    }

    /// 匯流排突然不見了（像重新登入、dbus-daemon 當掉）
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 開 dbus-daemon，等它印出位址（開始收連線了）才回來
fn spawn(conf: &std::path::Path) -> std::io::Result<Child> {
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", conf.display()))
        .args(["--nofork", "--print-address=1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(!line.trim().is_empty(), "dbus-daemon 沒有印出位址");
    Ok(child)
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
