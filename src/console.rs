//! Typing into a Claude Code session that runs in a terminal.
//!
//! Claude Code offers no public way to hand a prompt to an interactive session from outside,
//! so OYAKATA does what the user would do: it puts key presses into that terminal's console
//! input buffer (`AttachConsole` + `WriteConsoleInputW`). Only a process without a console can
//! attach to another one, so the typing happens in a short-lived copy of this executable that
//! is started without a console (`oyakata type-into`); the daemon's own console is untouched.

use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// What to type.
pub enum Keys {
    /// The prompt text, then Enter.
    Prompt(String),
    /// A single Esc, which interrupts Claude while it works.
    Escape,
}

/// The process to type into. `proc_start` is the creation time Claude Code recorded in
/// `~/.claude/sessions/<pid>.json`; when present it guards against a recycled pid.
pub struct Target {
    pub pid: u32,
    pub proc_start: Option<u64>,
}

/// Two sends at once would interleave their key presses.
static TYPING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run the helper and wait for it. Errors carry the helper's message.
pub async fn type_into(target: &Target, keys: Keys) -> Result<()> {
    let _turn = TYPING.lock().await;
    let exe = std::env::current_exe().context("locate the oyakata executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("type-into").arg("--pid").arg(target.pid.to_string());
    if let Some(t) = target.proc_start {
        cmd.arg("--proc-start").arg(t.to_string());
    }
    let text = match keys {
        Keys::Prompt(t) => t,
        Keys::Escape => {
            cmd.arg("--escape");
            String::new()
        }
    };
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    let mut child = cmd.spawn().context("start the typing helper")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await.context("hand the text to the typing helper")?;
    }
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .map_err(|_| anyhow!("typing helper did not finish within 15s"))??;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr.trim().trim_start_matches("Error: ").to_string();
        bail!("{}", if msg.is_empty() { format!("typing helper failed ({})", out.status) } else { msg });
    }
    Ok(())
}

/// Body of `oyakata type-into`: attach to the target's console and type.
pub fn helper_main(pid: u32, proc_start: Option<u64>, escape: bool) -> Result<()> {
    let text = if escape {
        String::new()
    } else {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).context("read the text from stdin")?;
        without_invisible(&s)
    };
    imp::type_keys(pid, proc_start, &text, escape)
}

/// Claude Code removes zero-width and bidi control characters from a prompt and then waits
/// for a second Enter, which nobody at the terminal would press. Drop them up front; emoji
/// joiners and variation selectors stay.
fn without_invisible(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(c,
                '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200E}' | '\u{200F}'
                | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}')
        })
        .collect()
}

#[cfg(windows)]
mod imp {
    use anyhow::{bail, Result};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, WriteConsoleInputW, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD,
        KEY_EVENT_RECORD_0, LEFT_CTRL_PRESSED,
    };
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_J: u16 = 0x4A;

    pub fn type_keys(pid: u32, proc_start: Option<u64>, text: &str, escape: bool) -> Result<()> {
        if let Some(expected) = proc_start {
            match creation_time(pid) {
                Some(t) if t == expected => {}
                Some(_) => bail!("{}", if crate::i18n::is_ja() { format!("pid {pid} は別のプロセスに再利用されています") } else { format!("pid {pid} was reused by another process") }),
                None => bail!("{}", if crate::i18n::is_ja() { format!("pid {pid} のプロセスが見つかりません") } else { format!("no process with pid {pid}") }),
            }
        }
        // SAFETY: plain Win32 calls; every pointer passed points at live local data.
        unsafe {
            FreeConsole();
            if AttachConsole(pid) == 0 {
                bail!("{}: {}", if crate::i18n::is_ja() { format!("pid {pid} のコンソールに接続できません") } else { format!("cannot attach to the console of pid {pid}") }, std::io::Error::last_os_error());
            }
            let name: Vec<u16> = "CONIN$\0".encode_utf16().collect();
            let input = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if input == INVALID_HANDLE_VALUE {
                let e = std::io::Error::last_os_error();
                FreeConsole();
                bail!("{}: {e}", crate::i18n::tr("コンソール入力を開けません", "cannot open the console input"));
            }
            let result = (|| -> Result<()> {
                if escape {
                    return write(input, &key(VK_ESCAPE, 0x01, 0x1B, 0));
                }
                let mut records = Vec::new();
                let text = text.replace("\r\n", "\n");
                for unit in text.trim_end().encode_utf16() {
                    // Ctrl+J is Claude Code's "new line without sending"; a bare CR would send.
                    if unit == u16::from(b'\n') {
                        records.extend(key(VK_J, 0x24, 0x0A, LEFT_CTRL_PRESSED));
                    } else if unit != u16::from(b'\r') {
                        records.extend(key(0, 0, unit, 0));
                    }
                }
                write(input, &records)?;
                // Arriving together with the text, Enter would count as part of a paste.
                std::thread::sleep(Duration::from_millis(250));
                write(input, &key(VK_RETURN, 0x1C, 0x0D, 0))
            })();
            CloseHandle(input);
            FreeConsole();
            result
        }
    }

    /// A key press and release.
    fn key(vk: u16, scan: u16, ch: u16, ctrl: u32) -> [INPUT_RECORD; 2] {
        let rec = |down: bool| INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: down as i32,
                    wRepeatCount: 1,
                    wVirtualKeyCode: vk,
                    wVirtualScanCode: scan,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: ch },
                    dwControlKeyState: ctrl,
                },
            },
        };
        [rec(true), rec(false)]
    }

    unsafe fn write(input: windows_sys::Win32::Foundation::HANDLE, records: &[INPUT_RECORD]) -> Result<()> {
        let mut done = 0usize;
        while done < records.len() {
            let mut n: u32 = 0;
            let chunk = &records[done..];
            if WriteConsoleInputW(input, chunk.as_ptr(), chunk.len() as u32, &mut n) == 0 || n == 0 {
                bail!("{}: {}", crate::i18n::tr("コンソールへの書き込みに失敗しました", "writing to the console failed"), std::io::Error::last_os_error());
            }
            done += n as usize;
        }
        Ok(())
    }

    fn creation_time(pid: u32) -> Option<u64> {
        // SAFETY: valid out-pointers; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
            let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
            CloseHandle(h);
            (ok != 0).then(|| (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::without_invisible;

    #[test]
    fn drops_zero_width_and_bom_but_keeps_emoji_sequences() {
        assert_eq!(without_invisible("\u{FEFF}a\u{200B}b\u{202E}c"), "abc");
        assert_eq!(without_invisible("👨\u{200D}👩 ❤\u{FE0F}"), "👨\u{200D}👩 ❤\u{FE0F}");
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub fn type_keys(_pid: u32, _proc_start: Option<u64>, _text: &str, _escape: bool) -> Result<()> {
        bail!("{}", crate::i18n::tr("ターミナルで稼働中のセッションへの送信は Windows でのみ使えます", "typing into a terminal session is only available on Windows"))
    }
}
