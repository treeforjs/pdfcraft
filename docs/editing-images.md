# Editing existing images and figures

In **Edit a PDF → Edit text and images**, click a figure to select it. Drag the selected
figure to move it, drag a corner to resize it proportionally, or press Delete to remove
that placement. The context menu also rotates or flips it, or crops it: **Crop Image** puts crop handles
on its edges and corners, and dragging one hides that side (Esc or Crop Image again ends
cropping). The hidden part is clipped, not discarded, so the crop moves, turns and flips with
the image, Replace Image keeps it, and undo brings the whole image back. Changes support undo and
survive saving/reopening.

PDF figures are not always bitmap images. Imported diagrams often use a **Form XObject**,
which packages text, vector paths, and nested images into one object. PdfCraft selects
this object using its `BBox` and optional `Matrix` and edits it as a whole. Its original
content, resources, clipping box, and transparency settings remain intact. Moving or
deleting one use of a shared figure does not alter its other placements or pages.

**Replace Image** and **Save Image As** apply only to raster Image XObjects; these menu
items are disabled for grouped Form artwork. The tool does not ungroup Forms, edit their
individual internal elements, select arbitrary ungrouped vector paths, or edit inline
images (`BI`/`ID`/`EI`). A Form's bounding box may include whitespace.

For automation, `page_images` lists both kinds with `kind: "image"` or `kind: "form"`.
Forms have `pixels: [0, 0]`, since they have no single bitmap resolution. Use the returned
number with `image_edit` (move, crop, rotate, flip_horizontal, flip_vertical, delete).
Coordinates are top-left-origin page points; `crop`'s `rect` is the part to keep. `image_save` and `image_edit`'s replace action
return a clear error for Form artwork.
