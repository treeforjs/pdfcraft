# Text recognition (OCR)

A scanned page is a picture: you can't search it, select its words or copy them. Text recognition reads the words in the picture and adds them to the page.

## Recognizing text

1. Open **All tools → Scan & OCR → In this file**.
2. Choose the pages: all pages, the current page, or a range.
3. Choose the document language.
4. Choose the output:
   - **Searchable Image** adds invisible text over each word. It can also straighten pages that are turned or skewed, and make very detailed pictures smaller (**Downsample to**).
   - **Searchable Image (Exact)** adds the invisible text and leaves the page picture exactly as it was.
5. Optionally choose **Review recognized text before applying**, then **Recognize text**.

Pages that already have text are skipped. Recognition runs in the background with a progress bar, and the result is one undo step.

## Checking the words

With **Review recognized text**, PdfCraft shows each word it was unsure about, next to the part of the page it read. Correct it, or leave it empty to leave it out, and go to the **Next word**. Then **Apply recognized text**.

## Many files at once

**In multiple files** recognizes text in a set of files and saves searchable copies in a folder you choose.

## Good to know

- Recognition works on your computer. Nothing is uploaded.
- The recognition models come with the desktop app. If they are missing, PdfCraft says so; reinstalling restores them. The web app downloads them (about 12 MB) the first time you recognize text, checks them, and keeps them in the browser for next time.
- The built-in models currently read the Latin alphabet without accents best (English). Other languages and scripts are planned.
- Documents that don't allow changes or copying can't be recognized unless you have their permissions password.
- An editable-text output, which replaces the picture of the words with real text in a matching font, size and colour, is available to automation (see [Automation](automation.md)) and not yet in the dialog.
- **Export to Word**, **Excel** and other formats can recognize scanned pages first, so the exported file has text.
