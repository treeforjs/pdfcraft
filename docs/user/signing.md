# Digital signatures

A digital signature proves who signed a document and shows whether it has changed since. It uses a **digital ID**: a certificate and a private key that belong to you. (For a picture of your handwritten signature, use Fill & Sign; see [Forms](forms.md).)

## Your digital ID

PdfCraft can sign with:

- a **.p12 or .pfx file** protected by a password;
- an identity in the **macOS Keychain** or the **Windows certificate store**;
- a **smart card or USB token** (**Sign with a smart card or token**), which asks for its PIN;
- a new **self-signed digital ID** that PdfCraft creates for you and saves to a password-protected file. Your name and organization from [Preferences](preferences.md) are suggested.

You can export the public part of a digital ID (its certificate) to give to the people who will check your signatures.

## Signing

1. Open **All tools → Use a certificate → Digitally sign**.
2. Drag a rectangle where the signature should appear, or click an empty signature field.
3. Choose your digital ID and enter its password.
4. Check the appearance, reason, location and contact details. Defaults for the reason and location come from [Preferences](preferences.md). Beside the name you can show a **drawn signature** (draw it with the mouse, a pen or a finger) or a **picture** of your signature, and add a faint **logo** behind it. **Save appearance** keeps the look under a name, so you can pick it next time.
5. Choose where to save the signed copy (PdfCraft suggests the name with `_signed` added).

A signature can also be invisible: it is stored in the file without anything on the page.

Signing can lock the fields of the form or the whole document, so nobody can change them after you sign: tick **Lock document after signing**.

## Certifying

**Certify (visible signature)** and **Certify (invisible signature)** sign as the document's author and say what others may still do: nothing, fill in forms and sign, or also add comments. People who open the document later see whether those rules were kept.

## Timestamps and long-term checking

- **Timestamp** adds a trusted time from a timestamp server, so the signing time can be proven later. Add, edit and remove servers in **Preferences → Signatures → Timestamp servers**; a server that needs an account asks for its password once per session.
- **Add verification information** (in the Signatures panel) makes every signature checkable for years, even after a certificate expires: PdfCraft fetches each certificate's revocation information from the addresses in the certificate, only from sites your internet access settings allow and only when you agree, and stores it in the document, optionally with a document timestamp.
- **Embed validation evidence** stores certificates and revocation information you already have as files (CRLs and OCSP responses).

## Checking signatures

PdfCraft checks every signature when a document opens. A bar above the page says whether the document is signed or certified and whether the signatures are valid.

- **Signatures** on the rail lists each signature with its status: valid, unknown (for example, the signer isn't trusted yet) or invalid.
- **Signature Properties** shows who signed, when, why and whether the document changed afterwards.
- **Changes after signing** lists what was changed in each later revision, and whether the signer allowed it.
- **View the signed version** opens the document as it was when it was signed. **Compare with current** opens it side by side with the current version, highlights what changed and says whether the signer allowed it.
- **Show certificate** opens the certificate viewer. **Add to trusted certificates** trusts a signer, so their future signatures are shown as valid. In **Preferences → Signatures → Trusted certificates** you can import certificates and choose what each is trusted for (as a root, for signed documents, for certified documents), and trust your operating system's root certificates.
- **Verification time** (Preferences → Signatures) chooses whether to check signatures at the timestamp time, the signing time or now.
- **Validate all signatures** checks them again, for example after you trusted a new certificate.
