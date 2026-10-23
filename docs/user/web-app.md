# The web app

PdfCraft also runs in a web browser, built from the same code as the desktop app. It works offline once loaded, and your documents never leave your computer: the browser opens them locally and nothing is uploaded.

## Opening and saving

- Open files with the **Open** button, or drag them onto the page.
- Saving downloads the file through your browser. Choose where your browser puts downloads in its own settings.
- A link to the web app can name a file to open (with `?file=` and the file's address). It opens in a tab in the background, so it doesn't interrupt what you are doing.

## Recovering work

Unsaved changes are copied into the browser's private storage for this site, at the interval set in [Preferences](preferences.md) (up to 256 MB in all). If the tab closes or the browser stops, PdfCraft offers to recover them the next time you open it. Saving or closing a document removes its copy. Clearing the site's data in your browser removes them too.

## Signing in the browser

You can sign and certify with a .p12 or .pfx digital ID that you pick from your computer. It is kept in memory for this page only, never in settings, and its password is never kept. You can also create a new digital ID, which downloads as a file, and check signatures. Fill & Sign works as on the desktop.

## What is different from the desktop app

- Documents open in tabs; separate windows are only on the desktop.
- Shortcuts are shown the macOS way (⌘). On Windows and Linux, press Ctrl instead.
- The macOS Keychain, the Windows certificate store and smart cards can't be used from a browser, and timestamp servers can't be reached from it.
- Text recognition (OCR) works, but the first time you use it the browser downloads its models (about 12 MB, with a progress bar and Cancel). They are kept for next time.
- Features that need your operating system, such as screen and window capture, are desktop-only.
- The command-line tool, the MCP server and the app control channel are desktop features (see [Automation](automation.md)).
