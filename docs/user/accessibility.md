# Accessibility

This page covers two things: making documents accessible to people who use screen readers and other assistive technology, and using PdfCraft itself in an accessible way.

## Checking a document

**All tools → Prepare for accessibility → Check for accessibility…** asks which pages and rules to check (**Options…**), then checks the document against them: whether it is tagged, has a title and a language, has alternate text for figures, has well-formed tables, lists and headings, has a tab order that follows the structure, and more. Some rules (such as colour contrast and reading order) need a person to look, and are marked that way.

The results open in the **Accessibility Checker** panel. For each problem you can:

- go to the content it is about;
- **Fix** it, when PdfCraft can (for example, set the title or the language);
- **Skip Rule**, or **Explain** what the rule asks for;
- **Check again** once you have fixed things.

**Open accessibility report** shows the full report, which you can also save.

## Fixing common problems

- **Add alternate text…** goes through the figures one by one so you can describe each, or mark it as decorative.
- **Change reading options…** sets how screen readers read the document.
- **File → Document properties…** sets the title, whether the window shows it instead of the file name, and the document's language.
- When you edit text, PdfCraft keeps the document's tags with it where it can.
- Exporting accessible text uses the document's tags, so the result follows its logical order.

Automatic tagging, a reading order tool and a guided "make accessible" action exist for automation (see [Automation](automation.md)) and are being brought into the app.

## Using PdfCraft with assistive technology

- Every control in PdfCraft has a name that screen readers announce.
- Everything can be reached from the keyboard. Press ⌘K to find any command by typing its name, and see [Keyboard shortcuts](keyboard.md).
- Under Accessibility in [Preferences](preferences.md):
  - **Text size** makes all interface text and controls larger or smaller.
  - **Reduce motion** turns off animations and smooth scrolling.
  - **High contrast** makes text, borders and outlines stronger in every theme.
- The Dark gray, Dim and Warm themes can be easier on the eyes for long reading sessions.
