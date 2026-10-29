# Preferences

**Edit → Preferences…** (⌘,) opens the settings. Categories are on the left. The search box at the top finds a setting on any page by name.

Settings are saved for you and apply to every document. They are never written into your PDFs.

## General

- **Interface language**: follow the system, or choose English, Japanese, Simplified or Traditional Chinese, Czech, Brazilian Portuguese, German, Spanish, French, Russian, Bulgarian, Telugu, Hungarian, Ukrainian or Italian. You can also press ⌘K and run **Interface language…**.
- **Theme**: Light gray, Warm, Dim, Dark gray, or the system setting.
- **Default workspace mode**: which tab (All tools, Read, Edit, Convert or E-Sign) documents open in.
- **Reopen the files that were open when PdfCraft last closed**.
- **Check for updates automatically**: off by default. Enabling it permits a daily check that only shows a notice. Manual checks remain available from Help. Existing saved choices are preserved.

## Documents

- **Save recovery copies of unsaved changes every** so many minutes. 0 turns this off.
- **Recent files to remember** on Home and in File → Open Recent. 0 keeps no list.
- **Default page display** and **Default zoom** for documents that don't ask for their own.

## Commenting

- **Name on new comments**. Empty uses your login name.
- **Keep the comment tool selected after adding a comment**.
- **Open the Comments panel when a comment tool is chosen**.

## Forms

- **Highlight form fields in documents that open**, with the **Field highlight color** and the **Required field border color**.
- **Date format** for date fields.
- **Flatten Fill & Sign when saving**: text, marks and signatures from Fill & Sign become part of the page.

## Identity

Your name, organization, unit and email address. They are suggested when you create a digital ID.

## Signatures

**Default reason** and **Default location** for new signatures. You can change them each time you sign.

- **Verify signatures using**: the timestamp time (else the signing time), the signing time, or the current time.
- **Fetch revocation information online** when adding verification information, and the **maximum age** of revocation information that has no update time (0 accepts any age).
- **Trust the operating system's root certificates** for signatures, and also for certified documents.
- **Trusted certificates**: import certificates and choose, for each, whether it is a trusted root and whether it is trusted for signed and certified documents.
- **Timestamp servers**: add, edit and remove servers, with an optional account name. Passwords are kept only until PdfCraft quits.
- **Signature appearances**: the appearances saved from the signing dialog; delete the ones you no longer need.

## Security

- **JavaScript**: let scripts in documents run, or turn them off.
- **Internet access from PDFs**: whether links and scripts in documents may reach web sites. Allow all sites, block all sites, or use site rules, and choose what happens with sites that have no rule (allow, block or ask each time).

## Accessibility

- **Text size**: larger or smaller interface text and controls.
- **Reduce motion**: no animations or smooth scrolling.
- **High contrast**: stronger text, borders and outlines.

## Color management

The RGB, CMYK and gray working spaces, the rendering intent, black point compensation, how untagged colour is treated, and when to use overprint preview. These matter mostly for print production.

## Settings that travel with you

Agents and scripts can read and change the same settings (see [Automation](automation.md)): `prefs_get` and `prefs_set` in the command-line tool, or `ui.set pref.<key>` through the app's control channel.
