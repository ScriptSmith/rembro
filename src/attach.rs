//! The page that tells you how to connect to the browser from inside the target.

use crate::browser::Family;
use crate::util::html_escape;

/// Example commands for connecting to `url` with each family's protocol.
pub fn commands(family: Family, url: &str) -> Vec<String> {
    match family {
        Family::Chromium => vec![
            format!("playwright-cli attach --cdp={url}"),
            format!("await chromium.connectOverCDP('{url}')  // Playwright"),
            format!("await puppeteer.connect({{ browserURL: '{url}' }})"),
        ],
        Family::Firefox => {
            vec![format!("await puppeteer.connect({{ browserWSEndpoint: '{url}', protocol: 'webDriverBiDi' }})")]
        }
        _ => Vec::new(),
    }
}

pub fn page(target: &str, protocol: &str, url: &str, commands: &[String]) -> String {
    let t = html_escape(target);
    let mut blocks = String::new();
    for c in commands {
        blocks.push_str(&format!(
            "  <div class=\"cmd\"><pre><code>{}</code></pre><button type=\"button\">Copy</button></div>\n",
            html_escape(c)
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>Connect from {t}</title>
<style>
  main {{ max-width: 46rem; margin: 10vh auto; padding: 0 1rem; font: 1rem/1.5 system-ui, sans-serif }}
  .cmd {{ display: flex; gap: .5rem; align-items: start; margin: .75rem 0 }}
  pre {{ flex: 1; margin: 0; padding: .75em 1em; border: 1px solid GrayText; white-space: pre-wrap; overflow-wrap: anywhere; user-select: all }}
  button {{ font: inherit; padding: .5em .9em }}
</style>
<main>
  <h1>Connect from {t}</h1>
  <p>Programs in {t} can drive this browser over {p} at <code>{u}</code>. For example:</p>
{blocks}  <p role="status"></p>
</main>
<script>
  for (const button of document.querySelectorAll('button')) {{
    button.onclick = async () => {{
      const code = button.parentElement.querySelector('code');
      const status = document.querySelector('[role=status]');
      try {{
        await navigator.clipboard.writeText(code.textContent);
        status.textContent = 'Copied.';
      }} catch {{
        getSelection().selectAllChildren(code);
        status.textContent = 'Press Ctrl+C to copy.';
      }}
    }};
  }}
</script>
"#,
        p = html_escape(protocol),
        u = html_escape(url),
    )
}
