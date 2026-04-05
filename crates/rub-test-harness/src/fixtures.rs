//! HTML fixture generators for test scenarios.

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
