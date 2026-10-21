# Desktop identity and existing installations

The current application identity is `io.github.treeforjs.pdfcraft`. Linux packages install the
current desktop entry and generate a hidden `ai.storyteller.pdfcraft.desktop` compatibility
entry, with matching icon aliases, for existing MIME associations and launchers. Both launch the
same `pdfcraft` executable. The compatibility entry does not create a second menu item.

The system package name stays `pdfcraft`, the Windows MSI upgrade code stays unchanged, and
settings, recovery files and digital identities keep their existing PdfCraft/pdfcraft paths.
Replacing an unpacked application folder does not require moving user documents or settings.

Flatpak application identities name separate sandboxes. An old Flatpak installation is not
silently removed, and installing the new identity does not erase the old sandbox. Close both
applications and back up the old sandbox's `data/pdfcraft` directory before copying settings to
the corresponding new sandbox. Verify the new installation before removing the old one. The
current release pipeline publishes native Windows and Linux packages, rather than Flatpak.

Required license authorship, upstream input provenance and PDF compatibility names remain in
the source and notices. Changing those bytes would misattribute dependencies or break existing
documents; they are not application branding.
