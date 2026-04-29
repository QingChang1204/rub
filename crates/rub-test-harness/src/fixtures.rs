//! HTML fixture generators for test scenarios.

/// Browser-backed download fixture owner used by E2E tests.
///
/// The server lifecycle is bound to the Rust value so grouped scenarios can
/// share one authority and release it deterministically during teardown.
pub use crate::download_fixture::DownloadFixtureServer;
/// Browser-backed network observation fixture owner used by E2E tests.
///
/// This is the harness authority for deterministic request traces and should be
/// preferred over ad hoc HTTP helpers inside individual test files.
pub use crate::network_fixture::NetworkInspectionFixtureServer;

/// A simple page with a title and links.
pub fn simple_page() -> &'static str {
    r##"<!DOCTYPE html>
<html>
<head><title>Test Page</title></head>
<body>
  <h1>Test Page</h1>
  <a href="/page2">Go to page 2</a>
  <button id="btn1">Click Me</button>
  <button id="btn2">Submit</button>
</body>
</html>"##
}

/// A form page with input fields.
pub fn form_page() -> &'static str {
    r##"<!DOCTYPE html>
<html>
<head><title>Form Page</title></head>
<body>
  <h1>Contact Form</h1>
  <form action="/submit" method="post">
    <input type="text" name="name" placeholder="Your name" />
    <input type="email" name="email" placeholder="Email address" />
    <textarea name="message" placeholder="Your message"></textarea>
    <select name="subject">
      <option value="general">General</option>
      <option value="support">Support</option>
    </select>
    <input type="checkbox" name="agree" /> I agree
    <button type="submit">Send</button>
  </form>
</body>
</html>"##
}

/// A long page for scroll testing.
pub fn long_page() -> &'static str {
    r##"<!DOCTYPE html>
<html>
<head><title>Long Page</title></head>
<body>
  <h1>Long Page</h1>
  <div style="height: 5000px;">
    <p>Top of page</p>
    <a href="#bottom">Jump to bottom</a>
  </div>
  <p id="bottom">Bottom of page</p>
  <a href="#top">Back to top</a>
</body>
</html>"##
}

/// A page that triggers a dialog on button click.
pub fn dialog_trigger_page() -> &'static str {
    r##"<!DOCTYPE html>
<html>
<head><title>Dialog Page</title></head>
<body>
  <h1>Dialog Test</h1>
  <button onclick="alert('Hello!')">Show Alert</button>
  <button onclick="confirm('Are you sure?')">Show Confirm</button>
  <button onclick="prompt('Enter value:')">Show Prompt</button>
</body>
</html>"##
}

/// A dynamic SPA-like page.
pub fn dynamic_spa_page() -> &'static str {
    r##"<!DOCTYPE html>
<html>
<head><title>SPA Page</title></head>
<body>
  <h1 id="title">Home</h1>
  <nav>
    <a href="#" onclick="navigate('about')">About</a>
    <a href="#" onclick="navigate('contact')">Contact</a>
  </nav>
  <div id="content">Welcome to the SPA</div>
  <script>
    function navigate(page) {
      document.getElementById('title').textContent = page;
      document.getElementById('content').textContent = 'Content: ' + page;
    }
  </script>
</body>
</html>"##
}

#[cfg(test)]
mod tests {
    use super::{dialog_trigger_page, dynamic_spa_page, form_page, long_page};
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rub-test-harness crate should live under workspace/crates")
            .parent()
            .expect("workspace root")
            .to_path_buf()
    }

    fn assert_root_fixture_matches_owner(path: &str, owner: fn() -> &'static str) {
        let path = workspace_root().join(path);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert_eq!(
            contents.trim_end_matches('\n'),
            owner(),
            "{} must stay byte-equivalent to its harness-owned fixture generator",
            path.display()
        );
    }

    #[test]
    fn root_fixture_files_match_harness_owned_generators() {
        assert_root_fixture_matches_owner("fixtures/dialog_trigger.html", dialog_trigger_page);
        assert_root_fixture_matches_owner("fixtures/dynamic_spa.html", dynamic_spa_page);
        assert_root_fixture_matches_owner("fixtures/form.html", form_page);
        assert_root_fixture_matches_owner("fixtures/long_page.html", long_page);
    }
}
