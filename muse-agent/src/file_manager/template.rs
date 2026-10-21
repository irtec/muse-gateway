use std::path::Path;

pub fn render_html_page(title: &str, h1: &str, crumb: &str, rows: &str, count: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600&family=Plus+Jakarta+Sans:wght@400;500;600;700&display=swap" rel="stylesheet">
<style>
:root {{
  --bg: #f8fafc;
  --surface: #ffffff;
  --surface-hover: #f1f5f9;
  --surface-active: #e2e8f0;
  --border: #e2e8f0;
  --border-focus: #0ea5e9;
  --text: #0f172a;
  --text-muted: #64748b;
  --text-faint: #94a3b8;
  --primary: #0284c7;
  --primary-hover: #0369a1;
  --primary-glow: rgba(2, 132, 199, 0.12);
  --header-bg: rgba(255, 255, 255, 0.85);
  --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --shadow-md: 0 4px 6px -1px rgba(0, 0, 0, 0.08), 0 2px 4px -2px rgba(0, 0, 0, 0.04);
  --font-sans: 'Plus Jakarta Sans', -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  --font-mono: 'JetBrains Mono', ui-monospace, Menlo, Monaco, Consolas, monospace;
}}

[data-theme="dark"] {{
  --bg: #090d16;
  --surface: #111827;
  --surface-hover: #1e293b;
  --surface-active: #273548;
  --border: #1f293d;
  --border-focus: #38bdf8;
  --text: #f1f5f9;
  --text-muted: #94a3b8;
  --text-faint: #475569;
  --primary: #38bdf8;
  --primary-hover: #7dd3fc;
  --primary-glow: rgba(56, 189, 248, 0.15);
  --header-bg: rgba(17, 24, 39, 0.85);
  --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.4);
  --shadow-md: 0 4px 12px -2px rgba(0, 0, 0, 0.5);
}}

* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  font-family: var(--font-sans);
  background: var(--bg);
  color: var(--text);
  min-height: 100vh;
  display: flex;
  flex-direction: column;
  -webkit-font-smoothing: antialiased;
}}

/* Top Navigation Bar */
header.app-bar {{
  background: var(--header-bg);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  z-index: 100;
  padding: 12px 24px;
}}
.bar-container {{
  max-width: 1280px;
  margin: 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}}
.brand-group {{
  display: flex;
  align-items: center;
  gap: 14px;
  min-width: 0;
}}
.brand-icon {{
  width: 36px;
  height: 36px;
  border-radius: 10px;
  background: linear-gradient(135deg, #0284c7 0%, #38bdf8 100%);
  display: flex;
  align-items: center;
  justify-content: center;
  color: #fff;
  flex-shrink: 0;
  box-shadow: var(--shadow-sm);
}}
.brand-info {{
  min-width: 0;
}}
.brand-info h1 {{
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.01em;
  color: var(--text);
  line-height: 1.2;
}}
.crumb-trail {{
  font-size: 12px;
  color: var(--text-muted);
  margin-top: 3px;
  display: flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.crumb-trail a {{
  color: var(--text-muted);
  text-decoration: none;
  transition: color 0.15s ease;
}}
.crumb-trail a:hover {{
  color: var(--primary);
}}
.crumb-sep {{
  color: var(--text-faint);
  font-size: 11px;
}}
.actions-group {{
  display: flex;
  align-items: center;
  gap: 8px;
}}
.btn-icon {{
  width: 38px;
  height: 38px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s ease;
}}
.btn-icon:hover {{
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--border-focus);
}}

/* Main Workspace */
main.workspace {{
  flex: 1;
  max-width: 1280px;
  width: 100%;
  margin: 0 auto;
  padding: 24px;
}}

/* Explorer Table Layout (Desktop) */
.explorer-card {{
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 16px;
  box-shadow: var(--shadow-md);
  overflow: hidden;
}}
.table-header {{
  display: grid;
  grid-template-columns: 48px 1fr 140px 180px 48px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--border);
  font-size: 12px;
  font-weight: 600;
  color: var(--text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}}
.table-body {{
  display: flex;
  flex-direction: column;
}}
.file-row {{
  display: grid;
  grid-template-columns: 48px 1fr 140px 180px 48px;
  align-items: center;
  padding: 12px 20px;
  border-bottom: 1px solid var(--border);
  text-decoration: none;
  color: inherit;
  transition: background-color 0.15s ease;
}}
.file-row:last-child {{
  border-bottom: none;
}}
.file-row:hover {{
  background: var(--surface-hover);
}}
.file-row:active {{
  background: var(--surface-active);
}}
.file-row.parent-row {{
  background: var(--surface-hover);
}}

/* File Badges */
.badge {{
  width: 36px;
  height: 36px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 11px;
  font-weight: 700;
  color: #fff;
  font-family: var(--font-mono);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.2);
}}
.b-dir {{ background: #0284c7; }}
.b-html {{ background: #7c3aed; }}
.b-zip {{ background: #d97706; }}
.b-img {{ background: #059669; }}
.b-md {{ background: #4f46e5; }}
.b-pdf {{ background: #e11d48; }}
.b-other {{ background: #64748b; }}

.col-name {{
  display: flex;
  flex-direction: column;
  padding-right: 16px;
  min-width: 0;
}}
.col-name b {{
  font-size: 14px;
  font-weight: 600;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.col-name span {{
  font-size: 12px;
  color: var(--text-muted);
  display: none; /* hidden on desktop table view */
}}
.col-size {{
  font-size: 13px;
  color: var(--text-muted);
  font-family: var(--font-mono);
}}
.col-date {{
  font-size: 13px;
  color: var(--text-muted);
}}
.col-action {{
  display: flex;
  justify-content: flex-end;
  color: var(--text-faint);
}}
.file-row:hover .col-action {{
  color: var(--primary);
}}

.empty-state {{
  text-align: center;
  padding: 64px 20px;
  color: var(--text-muted);
}}
.empty-state svg {{
  margin-bottom: 12px;
  color: var(--text-faint);
}}

/* Footer */
footer.app-footer {{
  padding: 16px 24px;
  max-width: 1280px;
  width: 100%;
  margin: 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
  color: var(--text-muted);
  border-top: 1px solid var(--border);
}}

/* Responsive Mobile Layout */
@media (max-width: 768px) {{
  header.app-bar {{ padding: 10px 16px; }}
  main.workspace {{ padding: 12px; }}
  .table-header {{ display: none; }}
  .file-row {{
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
  }}
  .col-name {{ flex: 1; }}
  .col-name span {{ display: block; margin-top: 2px; }}
  .col-size, .col-date {{ display: none; }}
  .col-action {{ margin-left: auto; }}
  footer.app-footer {{ flex-direction: column; gap: 8px; text-align: center; }}
}}
</style>
</head>
<body>
<header class="app-bar">
  <div class="bar-container">
    <div class="brand-group">
      <div class="brand-icon">
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"/></svg>
      </div>
      <div class="brand-info">
        <h1>{h1}</h1>
        <div class="crumb-trail">{crumb}</div>
      </div>
    </div>
    <div class="actions-group">
      <button class="btn-icon" onclick="toggleTheme()" aria-label="Toggle theme" title="Toggle Dark/Light">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/></svg>
      </button>
    </div>
  </div>
</header>

<main class="workspace">
  <div class="explorer-card">
    <div class="table-header">
      <div></div>
      <div>Name</div>
      <div>Size</div>
      <div>Last Modified</div>
      <div></div>
    </div>
    <div class="table-body">
      {rows}
    </div>
  </div>
</main>

<footer class="app-footer">
  <div>Muse Files Gateway • Native Rust & Axum</div>
  <div>{count}</div>
</footer>

<script>
(function(){{
  var t = localStorage.getItem('fm-theme');
  if(!t){{ t = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'; }}
  document.documentElement.setAttribute('data-theme', t);
  window.toggleTheme = function(){{
    var next = (document.documentElement.getAttribute('data-theme') === 'dark') ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', next);
    localStorage.setItem('fm-theme', next);
  }};
}})();
</script>
</body></html>"##,
        title = title,
        h1 = h1,
        crumb = crumb,
        rows = if rows.is_empty() {
            r#"<div class="empty-state">
              <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"/></svg>
              <div>This folder is empty</div>
            </div>"#
        } else {
            rows
        },
        count = count,
    )
}

pub fn render_markdown_page(
    title: &str,
    back: &str,
    raw_href: &str,
    raw_label: &str,
    body: &str,
) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} — Muse Docs</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600&family=Plus+Jakarta+Sans:wght@400;500;600;700&display=swap" rel="stylesheet">
<!-- Prism.js Syntax Highlighting Theme -->
<link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/prism/1.29.0/themes/prism-tomorrow.min.css">
<style>
:root {{
  --bg: #f8fafc;
  --surface: #ffffff;
  --border: #e2e8f0;
  --text: #0f172a;
  --text-muted: #475569;
  --text-faint: #94a3b8;
  --primary: #0284c7;
  --code-bg: #f1f5f9;
  --header-bg: rgba(255, 255, 255, 0.9);
  --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --shadow-md: 0 4px 6px -1px rgba(0, 0, 0, 0.08);
  --font-sans: 'Plus Jakarta Sans', -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  --font-mono: 'JetBrains Mono', ui-monospace, Menlo, Monaco, Consolas, monospace;
}}

[data-theme="dark"] {{
  --bg: #090d16;
  --surface: #111827;
  --border: #1f293d;
  --text: #f1f5f9;
  --text-muted: #94a3b8;
  --text-faint: #475569;
  --primary: #38bdf8;
  --code-bg: #1e293b;
  --header-bg: rgba(17, 24, 39, 0.9);
  --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.4);
  --shadow-md: 0 4px 12px -2px rgba(0, 0, 0, 0.5);
}}

* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  font-family: var(--font-sans);
  background: var(--bg);
  color: var(--text);
  min-height: 100vh;
  line-height: 1.7;
  -webkit-font-smoothing: antialiased;
}}

/* Top Bar */
header.doc-bar {{
  background: var(--header-bg);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  z-index: 100;
  padding: 12px 24px;
}}
.doc-bar-container {{
  max-width: 1024px;
  margin: 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}}
.doc-nav {{
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}}
.btn-back {{
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  text-decoration: none;
  font-size: 18px;
  transition: all 0.15s ease;
}}
.btn-back:hover {{
  background: var(--code-bg);
  border-color: var(--primary);
  color: var(--primary);
}}
.doc-title {{
  font-size: 15px;
  font-weight: 700;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.doc-actions {{
  display: flex;
  align-items: center;
  gap: 8px;
}}
.btn-raw {{
  background: var(--surface);
  border: 1px solid var(--border);
  color: var(--text);
  padding: 6px 14px;
  border-radius: 8px;
  font-size: 12px;
  font-weight: 600;
  text-decoration: none;
  transition: all 0.15s ease;
}}
.btn-raw:hover {{
  border-color: var(--primary);
  color: var(--primary);
}}
.btn-icon {{
  width: 36px;
  height: 36px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s ease;
}}
.btn-icon:hover {{
  border-color: var(--primary);
  color: var(--text);
}}

/* Article Container */
main.markdown-body {{
  max-width: 1024px;
  margin: 32px auto 64px;
  padding: 40px 48px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 16px;
  box-shadow: var(--shadow-md);
}}

/* Typography */
.markdown-body h1, .markdown-body h2, .markdown-body h3, .markdown-body h4 {{
  color: var(--text);
  font-weight: 700;
  line-height: 1.3;
  margin-top: 1.8em;
  margin-bottom: 0.6em;
}}
.markdown-body h1 {{
  font-size: 2.1em;
  border-bottom: 1px solid var(--border);
  padding-bottom: 0.3em;
  margin-top: 0;
}}
.markdown-body h2 {{
  font-size: 1.5em;
  border-bottom: 1px solid var(--border);
  padding-bottom: 0.3em;
}}
.markdown-body h3 {{ font-size: 1.25em; }}
.markdown-body h4 {{ font-size: 1.05em; }}
.markdown-body p {{ margin: 0.9em 0; }}
.markdown-body a {{ color: var(--primary); text-decoration: none; }}
.markdown-body a:hover {{ text-decoration: underline; }}
.markdown-body ul, .markdown-body ol {{ margin: 0.8em 0 0.8em 1.8em; }}
.markdown-body li {{ margin: 0.3em 0; }}
.markdown-body hr {{ border: none; border-top: 1px solid var(--border); margin: 2em 0; }}
.markdown-body blockquote {{
  border-left: 4px solid var(--primary);
  background: var(--code-bg);
  padding: 12px 18px;
  margin: 1.2em 0;
  border-radius: 0 8px 8px 0;
  color: var(--text-muted);
}}

/* Inline Code */
.markdown-body :not(pre) > code {{
  font-family: var(--font-mono);
  font-size: 0.88em;
  background: var(--code-bg);
  color: var(--text);
  padding: 3px 6px;
  border-radius: 6px;
  border: 1px solid var(--border);
}}

/* Code Block with Header & Copy Button */
.code-wrapper {{
  position: relative;
  margin: 1.4em 0;
  border-radius: 12px;
  overflow: hidden;
  border: 1px solid var(--border);
  background: #181c24; /* sleek dark code surface */
  box-shadow: var(--shadow-sm);
}}
.code-header {{
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 16px;
  background: #12151c;
  border-bottom: 1px solid rgba(255,255,255,0.06);
  font-family: var(--font-mono);
  font-size: 11px;
  color: #94a3b8;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}}
.btn-copy {{
  display: inline-flex;
  align-items: center;
  gap: 5px;
  background: rgba(255,255,255,0.08);
  border: 1px solid rgba(255,255,255,0.12);
  color: #cbd5e1;
  padding: 4px 10px;
  border-radius: 6px;
  font-size: 11px;
  font-family: var(--font-sans);
  font-weight: 500;
  cursor: pointer;
  transition: all 0.15s ease;
}}
.btn-copy:hover {{
  background: rgba(255,255,255,0.16);
  color: #fff;
}}
.code-wrapper pre {{
  margin: 0 !important;
  padding: 16px 20px !important;
  background: transparent !important;
  border: none !important;
  overflow-x: auto;
}}
.code-wrapper pre code {{
  background: transparent !important;
  border: none !important;
  padding: 0 !important;
  font-family: var(--font-mono) !important;
  font-size: 13.5px !important;
  line-height: 1.6 !important;
  color: #e2e8f0 !important;
  white-space: pre !important;
  word-break: normal !important;
  overflow-wrap: normal !important;
}}

/* Tables */
.table-wrapper {{
  overflow-x: auto;
  margin: 1.4em 0;
  border: 1px solid var(--border);
  border-radius: 12px;
}}
.markdown-body table {{
  width: 100%;
  border-collapse: collapse;
  font-size: 14px;
}}
.markdown-body th, .markdown-body td {{
  padding: 10px 14px;
  border-bottom: 1px solid var(--border);
  text-align: left;
}}
.markdown-body th {{
  background: var(--code-bg);
  font-weight: 600;
}}
.markdown-body tr:last-child td {{
  border-bottom: none;
}}

@media (max-width: 768px) {{
  header.doc-bar {{ padding: 10px 16px; }}
  main.markdown-body {{ margin: 16px; padding: 20px 16px; }}
}}
</style>
</head>
<body>
<header class="doc-bar">
  <div class="doc-bar-container">
    <div class="doc-nav">
      <a class="btn-back" href="{back}" title="Back to folder">‹</a>
      <div class="doc-title">{title}</div>
    </div>
    <div class="doc-actions">
      <a class="btn-raw" href="{raw_href}">{raw_label}</a>
      <button class="btn-icon" onclick="toggleTheme()" aria-label="Toggle theme">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/></svg>
      </button>
    </div>
  </div>
</header>

<main class="markdown-body">
  {body}
</main>

<!-- Prism JS & Autoloader for syntax highlight -->
<script src="https://cdnjs.cloudflare.com/ajax/libs/prism/1.29.0/prism.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/prism/1.29.0/plugins/autoloader/prism-autoloader.min.js"></script>
<script>
Prism.plugins.autoloader.languages_path = 'https://cdnjs.cloudflare.com/ajax/libs/prism/1.29.0/components/';

(function(){{
  var t = localStorage.getItem('fm-theme');
  if(!t){{ t = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'; }}
  document.documentElement.setAttribute('data-theme', t);
  window.toggleTheme = function(){{
    var next = (document.documentElement.getAttribute('data-theme') === 'dark') ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', next);
    localStorage.setItem('fm-theme', next);
  }};
}})();

function copyCode(btn) {{
  var code = btn.closest('.code-wrapper').querySelector('code').innerText;
  navigator.clipboard.writeText(code).then(function(){{
    var orig = btn.innerHTML;
    btn.innerHTML = 'Copied!';
    btn.style.color = '#38bdf8';
    setTimeout(function(){{
      btn.innerHTML = orig;
      btn.style.color = '';
    }}, 1500);
  }});
}}
</script>
</body></html>"##,
        title = title,
        back = back,
        raw_href = raw_href,
        raw_label = raw_label,
        body = body,
    )
}

pub fn render_markdown_content(raw_text: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(raw_text, options);
    let mut custom_events = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buffer = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                code_buffer.clear();
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => "".to_string(),
                };
            }
            Event::Text(text) if in_code_block => {
                code_buffer.push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let lang_clean = code_lang.trim();
                let prism_lang = if lang_clean.is_empty() {
                    "text"
                } else if lang_clean.eq_ignore_ascii_case("bash")
                    || lang_clean.eq_ignore_ascii_case("sh")
                {
                    "bash"
                } else if lang_clean.eq_ignore_ascii_case("toml") {
                    "toml"
                } else if lang_clean.eq_ignore_ascii_case("rust")
                    || lang_clean.eq_ignore_ascii_case("rs")
                {
                    "rust"
                } else if lang_clean.eq_ignore_ascii_case("json") {
                    "json"
                } else if lang_clean.eq_ignore_ascii_case("python")
                    || lang_clean.eq_ignore_ascii_case("py")
                {
                    "python"
                } else {
                    lang_clean
                };

                let label = if lang_clean.is_empty() {
                    "CODE"
                } else {
                    lang_clean
                };
                let esc_code = html_escape(&code_buffer);

                let wrapper_html = format!(
                    r##"<div class="code-wrapper">
  <div class="code-header">
    <span>{}</span>
    <button class="btn-copy" onclick="copyCode(this)">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
      Copy
    </button>
  </div>
  <pre><code class="language-{}">{}</code></pre>
</div>"##,
                    label, prism_lang, esc_code
                );
                custom_events.push(Event::Html(wrapper_html.into()));
            }
            Event::Start(Tag::Table(_)) => {
                custom_events.push(Event::Html(r#"<div class="table-wrapper">"#.into()));
                custom_events.push(event);
            }
            Event::End(TagEnd::Table) => {
                custom_events.push(event);
                custom_events.push(Event::Html(r#"</div>"#.into()));
            }
            _ => {
                if !in_code_block {
                    custom_events.push(event);
                }
            }
        }
    }

    let mut html_output = String::new();
    html::push_html(&mut html_output, custom_events.into_iter());
    html_output
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

pub fn get_badge(filename: &str, is_dir: bool) -> (&'static str, &'static str) {
    if is_dir {
        return ("b-dir", "DIR");
    }
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "html" | "htm" => ("b-html", "HTML"),
        "zip" | "tar" | "gz" | "bz2" | "7z" => ("b-zip", "ZIP"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => ("b-img", "IMG"),
        "md" => ("b-md", "MD"),
        "pdf" => ("b-pdf", "PDF"),
        _ => ("b-other", "FILE"),
    }
}
