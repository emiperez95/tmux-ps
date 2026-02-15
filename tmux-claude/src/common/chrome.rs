//! Chrome tab detection via AppleScript — fetches tabs and matches to listening ports.

use crate::common::ports::ListeningPort;
use std::process::Command;

/// A Chrome browser tab.
#[derive(Debug, Clone)]
pub struct ChromeTab {
    pub title: String,
    pub url: String,
    pub window_index: usize,
    pub tab_index: usize,
}

/// Get all Chrome tabs via AppleScript.
///
/// Returns empty vec if Chrome is not running or AppleScript fails.
/// Each tab is returned with its window index, tab index, title, and URL.
pub fn get_chrome_tabs() -> Vec<ChromeTab> {
    let script = r#"
tell application "System Events"
    if not (exists process "Google Chrome") then return ""
end tell
tell application "Google Chrome"
    set output to ""
    repeat with w from 1 to count of windows
        repeat with t from 1 to count of tabs of window w
            set tabTitle to title of tab t of window w
            set tabURL to URL of tab t of window w
            set output to output & w & "\t" & t & "\t" & tabTitle & "\t" & tabURL & "\n"
        end repeat
    end repeat
    return output
end tell
"#;

    let output = match Command::new("osascript").arg("-e").arg(script).output() {
        Ok(out) => out,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_chrome_tabs(&stdout)
}

/// Parse AppleScript output into ChromeTab structs.
fn parse_chrome_tabs(output: &str) -> Vec<ChromeTab> {
    let mut tabs = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 4 {
            continue;
        }

        let window_index = match parts[0].parse::<usize>() {
            Ok(w) => w,
            Err(_) => continue,
        };
        let tab_index = match parts[1].parse::<usize>() {
            Ok(t) => t,
            Err(_) => continue,
        };

        tabs.push(ChromeTab {
            title: parts[2].to_string(),
            url: parts[3].to_string(),
            window_index,
            tab_index,
        });
    }

    tabs
}

/// Match Chrome tabs to a set of listening ports.
///
/// A tab matches if its URL contains `localhost:PORT` or `127.0.0.1:PORT`.
pub fn match_tabs_to_ports(tabs: &[ChromeTab], ports: &[ListeningPort]) -> Vec<(ChromeTab, u16)> {
    let mut matched = Vec::new();

    for tab in tabs {
        for port in ports {
            let port_str = port.port.to_string();
            // Check common localhost patterns in URL
            if tab.url.contains(&format!("localhost:{}", port_str))
                || tab.url.contains(&format!("127.0.0.1:{}", port_str))
                || tab.url.contains(&format!("[::1]:{}", port_str))
            {
                matched.push((tab.clone(), port.port));
                break; // One match per tab is enough
            }
        }
    }

    matched
}

/// Open a URL in Chrome (new tab).
pub fn open_chrome_tab(url: &str) -> bool {
    std::process::Command::new("open")
        .args(["-a", "Google Chrome", url])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Focus a specific Chrome tab by activating its window and setting the active tab index.
pub fn focus_chrome_tab(tab: &ChromeTab) -> bool {
    let script = format!(
        r#"
tell application "Google Chrome"
    set active tab index of window {} to {}
    set index of window {} to 1
    activate
end tell
"#,
        tab.window_index, tab.tab_index, tab.window_index
    );

    Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chrome_tabs_empty() {
        assert!(parse_chrome_tabs("").is_empty());
    }

    #[test]
    fn test_parse_chrome_tabs_single() {
        let input = "1\t1\tMy App\thttp://localhost:3000/\n";
        let tabs = parse_chrome_tabs(input);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].window_index, 1);
        assert_eq!(tabs[0].tab_index, 1);
        assert_eq!(tabs[0].title, "My App");
        assert_eq!(tabs[0].url, "http://localhost:3000/");
    }

    #[test]
    fn test_parse_chrome_tabs_multiple() {
        let input = "1\t1\tTab One\thttp://localhost:3000/\n\
                      1\t2\tTab Two\thttp://example.com\n\
                      2\t1\tTab Three\thttp://127.0.0.1:8080/api\n";
        let tabs = parse_chrome_tabs(input);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[2].window_index, 2);
        assert_eq!(tabs[2].url, "http://127.0.0.1:8080/api");
    }

    #[test]
    fn test_match_tabs_to_ports_localhost() {
        let tabs = vec![
            ChromeTab {
                title: "My App".into(),
                url: "http://localhost:3000/".into(),
                window_index: 1,
                tab_index: 1,
            },
            ChromeTab {
                title: "Google".into(),
                url: "https://google.com".into(),
                window_index: 1,
                tab_index: 2,
            },
        ];
        let ports = vec![ListeningPort {
            port: 3000,
            pid: 123,
            process_name: "node".into(),
        }];
        let matched = match_tabs_to_ports(&tabs, &ports);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0.title, "My App");
        assert_eq!(matched[0].1, 3000);
    }

    #[test]
    fn test_match_tabs_to_ports_127() {
        let tabs = vec![ChromeTab {
            title: "API".into(),
            url: "http://127.0.0.1:8080/api".into(),
            window_index: 1,
            tab_index: 1,
        }];
        let ports = vec![ListeningPort {
            port: 8080,
            pid: 456,
            process_name: "java".into(),
        }];
        let matched = match_tabs_to_ports(&tabs, &ports);
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn test_match_tabs_no_match() {
        let tabs = vec![ChromeTab {
            title: "Google".into(),
            url: "https://google.com".into(),
            window_index: 1,
            tab_index: 1,
        }];
        let ports = vec![ListeningPort {
            port: 3000,
            pid: 123,
            process_name: "node".into(),
        }];
        let matched = match_tabs_to_ports(&tabs, &ports);
        assert!(matched.is_empty());
    }
}
