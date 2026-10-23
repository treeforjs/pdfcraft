# Forms

## Filling in a form

- Click a field and type. Press Tab to go to the next field and ⇧Tab to go back.
- Click checkboxes and radio buttons to choose them; choose from lists and drop-down lists.
- Date fields offer a date picker. Number, currency, percentage, date, phone and postcode fields are formatted as you leave them, and values outside the allowed range are refused with a message.
- Fields that add up other fields recalculate as you type.
- Required fields are outlined. Field highlighting can be turned on and off per document, and its colours are set in [Preferences](preferences.md).
- **Form fields** on the rail lists every field and its value.
- **Edit → Clear form** empties every field.

Some forms run small scripts, for example to check a value or fill in a total. PdfCraft runs them in a safe sandbox. You can turn scripts off under Security in [Preferences](preferences.md).

When a form has a submit button, PdfCraft shows you where the data will be sent and asks before it sends anything.

## Saving form data on its own

- **Prepare a form → Export data** saves just the answers (FDF, XFDF, XML, CSV or tab-separated text). **Import data** fills a form from such a file.
- **Merge data files into spreadsheet…** collects the answers from many filled-in forms into one CSV file.

## Fill & Sign

For forms that have no fields (a scanned letter, say), open the **E-Sign** tab or **All tools → Fill & Sign**:

- **Add text** anywhere on the page.
- **Checkmark**, **Cross**, **Dot** and **Line** for boxes and blanks.
- **Date** adds today's date.
- **Signature** and **Initials**: type, draw or use a picture of your signature once; PdfCraft remembers it for next time. **Change signature** or **remove saved signature** when you need to.

What you add with Fill & Sign can be made part of the page when you save (see Forms in [Preferences](preferences.md)). A Fill & Sign signature is a picture of your signature. To sign with a certificate that proves who signed, see [Digital signatures](signing.md).

## Building a form

Open **All tools → Prepare a form**.

- **Detect form fields** looks for blanks and boxes on the page and adds fields for them, named after the labels next to them.
- **Add form components**: text fields, checkboxes, radio buttons, drop-down lists, list boxes, buttons, image fields, date fields and digital signature fields. Drag a rectangle on the page to place one.
- Select fields to move, resize, align, centre, distribute or match their sizes. Right-click a field to duplicate it across pages.
- **Field properties** sets the name, tooltip, whether it is required or read-only, its look, its options, its format (number, date, phone…), validation, calculation and actions.
- **Tab order by rows** or **by columns** (or by the document's structure) sets the order Tab follows.
- **Flatten form fields** turns the fields and their values into part of the page.

Forms made with XML Forms Architecture (XFA) open too, and can be filled in, although not every XFA feature is supported yet.

## Scripts in forms

Developers can look at and change a document's scripts:

- **Document JavaScripts…** lists the scripts stored in the document.
- **JavaScript console…** (⌘J) runs script lines against the open document.
- **JavaScript debugger…** (⇧⌘J) steps through scripts with breakpoints and watches.
