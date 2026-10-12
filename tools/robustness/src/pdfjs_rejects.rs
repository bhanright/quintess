//! Files in the pdf.js corpus that are broken on purpose and that pdf.js
//! itself refuses, wholly or in part. When Quintess fails on one of these,
//! the sweep counts it as an expected failure rather than an error.
//!
//! The list comes from pdf.js's own unit tests (`test/unit/api_spec.js` at
//! the pinned commit), each of which expects an exception for the file.
//! Add to it only with such evidence: a file pdf.js renders is a file
//! Quintess should open.

/// `(file name, what pdf.js's test expects)`.
pub const PDFJS_REJECTS: &[(&str, &str)] = &[
    ("bug1020226.pdf", "\"creates pdf doc from invalid PDF file\" expects InvalidPDFException"),
    ("REDHAT-1531897-0.pdf", "\"creates pdf doc from PDF file with bad XRef byteWidths\" expects InvalidPDFException"),
    ("poppler-395-0-fuzzed.pdf", "\"creates pdf doc from PDF file with inaccessible /Pages tree\" expects InvalidPDFException"),
    ("poppler-85140-0.pdf", "\"bad /Pages tree /Count\": page 1 fails with \"Bad (uncompressed) XRef entry: 3R\" when stopping at errors"),
    ("poppler-742-0-fuzzed.pdf", "\"bad /Pages tree /Kids entries\" expects UnknownErrorException for a page"),
    ("poppler-937-0-fuzzed.pdf", "\"bad /Pages tree /Kids entries\" expects UnknownErrorException for a page"),
    ("Pages-tree-refs.pdf", "\"gets page, from /Pages tree with circular reference\": page 2 fails with \"Pages tree contains circular reference\""),
];

/// Why pdf.js rejects `file_name`, if it is one of the files above.
pub fn pdfjs_rejection(file_name: &str) -> Option<&'static str> {
    PDFJS_REJECTS.iter().find(|(name, _)| *name == file_name).map(|(_, reason)| *reason)
}
