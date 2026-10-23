# Security and redaction

## Opening protected documents

When a document needs a password to open, PdfCraft asks for it. Documents can also restrict what you may do (print, change, copy, add comments, fill in forms). PdfCraft honours those restrictions and shows a bar above the page that says what is restricted. If you have the document's permissions password, entering it lifts the restrictions.

**File → Security properties…** shows how a document is protected, which password opened it, and what each permission allows.

## Adding a password

**File → Protect using password…** (or **All tools → Protect a PDF**):

- **A password to open**: anyone opening it needs this password.
- **A permissions password**: the document opens normally, but a permissions password is needed to change the restrictions. Choose whether printing is allowed (not at all, low resolution or high resolution), which changes are allowed, whether text and images may be copied, and whether screen readers may read the text.
- **Compatibility** chooses the encryption. The newest (256-bit AES) is the strongest; older ones are only for very old PDF readers.
- You can leave the document's metadata unencrypted so search tools can still index it.

A strength meter helps you choose a good password. The protection is applied when you save.

**File → Remove security** takes the protection off (you need the permissions password if there is one).

## Redaction

Redaction removes content for good: the text, pictures and drawings under a redaction mark are deleted from the file, not just covered.

1. Open **All tools → Redact a PDF**.
2. Mark what to remove:
   - **Redact text and images**: drag across text, or draw a box around anything.
   - **Redact pages…**: whole pages or a range.
   - **Find text and redact…**: a word or phrase, a list of words (one per line, or imported from a file), or patterns such as phone numbers, email addresses, card numbers, national ID numbers and dates.
3. **Set properties** chooses the fill colour of redacted areas and optional overlay text, such as a redaction code.
4. **Apply redactions…** removes everything under the marks, after asking you to confirm. Comments and form fields that overlap the marks are removed too.
5. Save the document. Saving writes the whole file again, so no trace of the removed content stays in it.

Text that is part of a scanned picture can't be found by searching. Recognise the text first (see [Text recognition](ocr.md)).

## Hidden information

- **Remove hidden information…** lists what the document carries besides its visible pages (metadata, attachments, comments, form fields, hidden text and layers, bookmarks, links, scripts, private application data, and more) and removes the items you choose.
- **Sanitize document…** removes all of it at once. Use it before sharing a document outside your organization.

## Attachments and web links

PdfCraft won't open attachments of risky types (such as programs). Whether links and scripts in documents may reach web sites is set under Security in [Preferences](preferences.md): allow all, block all, or ask, with rules for particular sites.
