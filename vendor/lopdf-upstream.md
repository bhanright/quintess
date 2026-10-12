# Drafts of lopdf bug reports

Drafts for https://github.com/J-F-Liu/lopdf, one per patch Quintess carries
in `vendor/lopdf` (see vendor/README.md, Q1 to Q8). **None is filed yet.**
Each can go as an issue, or as a pull request with the patch from the
vendored source and the matching test from
`crates/quintess-core/tests/lopdf_patches.rs` rewritten against lopdf
alone. Line numbers are lopdf 0.45.0's. The PdfCraft patches (P1 to P5)
are PdfCraft's to report; P2 and P3 overlap lopdf#589.

---

## 1. Password accepted but contents scrambled: the file key is made from the unprepared password

**lopdf 0.45.0, `src/reader.rs`, `Reader::authenticate_and_setup_encryption`**

`Document::authenticate_password` prepares the password with
`PasswordAlgorithm::sanitize_password` (SASLprep and UTF-8 for revisions 5
and 6, PDFDocEncoding for 2 to 4) before checking it. But
`authenticate_and_setup_encryption` then passes the password *as typed* to
`EncryptionState::decode`, which makes the file key from the raw bytes. When
preparation changes the password, the check passes, the key is wrong, and the
document loads with every string and stream still encrypted, without an
error.

Reproduce: pdf.js's `test/pdfs/saslprep-r6.pdf`, password `"S\u{aa}SL\u{ad}prep"`
(SASLprep maps U+00AA to "a" and removes the soft hyphen). Or any AES-128
file whose password has a non-ASCII character such as "æøå": PDFDocEncoding
gives three bytes, UTF-8 six.

Fix: prepare the password before decoding, as `Document::decrypt` already does:

```rust
let prepared = PasswordAlgorithm::try_from(&self.document)?.sanitize_password(password)?;
let state = EncryptionState::decode(&self.document, prepared)?;
```

---

## 2. Revisions 2 to 4: opening with the owner password gives a wrong file key

**`src/encryption/algorithms.rs`, `PasswordAlgorithm::compute_file_encryption_key`**

For revisions 2 to 4 the file key is computed from the *user* password
(Algorithm 2). `authenticate_password` accepts the owner password (Algorithm
7), but the key is then computed from the owner password as if it were the
user password, so the contents come out scrambled.

Reproduce: encrypt a document with `EncryptionVersion::V4` (or V2), user
password "user", owner password "owner"; load it with
`LoadOptions::with_password("owner")`; the page content is garbage. With
"user" it is correct.

Fix: when the password doesn't authenticate as the user password, recover
the user password from /O (the first steps of Algorithm 7, which
`authenticate_owner_password_r4` already performs) and compute the key from
that. Quintess's patch splits those steps into
`user_password_from_owner_password_r4` and uses it in both places.

---

## 3. An encryption dictionary written directly in the trailer is ignored

**`src/reader.rs` (`parse_encryption_dictionary`) and `src/document.rs` (`get_encrypted`)**

`/Encrypt` may be a direct dictionary in the trailer (PDF 32000-1 doesn't
require an indirect object; some producers write it inline). lopdf only
looks for `/Encrypt N G R`, so `is_encrypted()` is false, the document loads
as if unencrypted, and every string and stream stays scrambled; callers that
check `is_encrypted()` later see an encrypted document whose password is
"not accepted".

Reproduce: pdf.js's `test/pdfs/issue6010_1.pdf` (password "abc") and
`issue6010_2.pdf` (password "æøå"), both AES-256 revision 6.

Fix: on loading, move a direct `/Encrypt` dictionary into an object under an
unused number and point the trailer at it, so the rest of the reader works
unchanged (it is dropped after decryption, as the indirect one is).

---

## 4. Crypt filters given by reference (`/CF 7 0 R`) are not found

**`src/reader.rs` (`parse_encryption_dictionary`), `src/document.rs` (`get_crypt_filters`)**

For an encrypted file the reader loads only the encryption dictionary before
setting up decryption. If `/CF`, or a crypt filter inside it, is a reference
to another object, `get_crypt_filters` (which calls `as_dict()` without
dereferencing) finds no filters; `get_stream_filter` then falls back to RC4
and the AES-encrypted contents are left undecrypted, without an error.

Reproduce: pdf.js's `test/pdfs/issue7665.pdf` (AES-256, empty user
password, `/CF 7 0 R`).

Fix: when parsing the encryption dictionary, replace references inside it
with the objects they name (they hold only names and numbers, so they need
no decryption), and dereference in `get_crypt_filters` for documents whose
objects are all loaded.

---

## 5. Whitespace before `endstream` drops the stream

**`src/parser/mod.rs`, `stream` and `recover_stream_length`**

After `take(length)` the parser accepts only an optional end of line before
`endstream`. Real files put other whitespace there:
`stream\r\nq endstream` with `/Length 1` (pdf.js's linked `issue1721.pdf`).
The stream then fails to parse and the object is dropped, so the page loses
part of its content and saving writes a reference to nothing.

Also, length recovery requires `endstream` to follow an end of line
directly, so an indented `\r\n\tendstream` with a wrong `/Length` is not
recovered (pdf.js's linked `issue5592.pdf`).

Fix: accept any run of PDF whitespace between the data and `endstream`; in
`recover_stream_length`, skip spaces and tabs before checking for the end of
line.

---

## 6. A damaged latest copy of an object loses the object; fall back to an earlier revision

**`src/reader.rs`, `load_objects_raw`**

When the object at the offset the (latest) cross-reference section gives
fails to parse, lopdf drops it. In incrementally updated files the previous
revision's copy is often intact, and PDFium uses it. pdf.js's linked
`issue13783.pdf` has a page whose last copy is spliced with another object's
bytes (`/MediaBox [ 0 Resources 26 0 R ...`); lopdf finds 14 pages, PDFium 15.

Fix: on a parse failure (lenient mode, unencrypted), scan the file for the
object's headers once (`scan_object_markers`, already used by reconstruction)
and use the newest other copy that parses with the expected id.

---

## 7. Arithmetic overflow on a cross-reference stream `/Index` start near `i64::MAX`

**`src/parser_aux.rs:633`, `decode_xref_stream`**

`(start + j) as u32` overflows (a panic with overflow checks, as in debug
builds) when `/Index` starts near `i64::MAX`; in release it wraps silently.
Found by mutation fuzzing.

Reproduce: an xref stream with `/Index [9223372036854775806 2] /W [1 2 1]`
and two entries.

Fix: `start.checked_add(j).and_then(|n| u32::try_from(n).ok())`, reading
but skipping entries whose number doesn't fit.

---

## 8. `prune_objects` is quadratic

**`src/document.rs` (`traverse_objects`), `src/processor.rs` (`prune_objects`)**

`traverse_objects` keeps the references found in a `Vec` and calls
`contains` for each reference; `prune_objects` calls `contains` on that list
for every object. On a 100,000-object file (pdf.js's linked `pdf.pdf`) one
prune took 50 seconds.

Fix: keep a `HashSet` beside the list (the returned order is unchanged) and
look ids up in a set in `prune_objects`.
