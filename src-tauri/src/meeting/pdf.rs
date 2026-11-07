//! PDFs read and marked on this Mac, through the PDFKit bridge.

use serde::Serialize;

/// The largest PDF a meeting keeps. The file is read whole and stored in the
/// meeting library; a contract or a deck is a small fraction of this.
pub(crate) const MAX_PDF_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PdfError {
    /// Not a PDF, a locked one, or one with no text to read.
    Unreadable,
    WriteFailed,
}

/// One passage to highlight, on a page counted from 0, with the note that
/// goes beside it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PdfMark {
    pub page: usize,
    pub passage: String,
    pub note: String,
}

/// A marked copy's bytes and the notes it holds, read back from them.
pub(crate) struct MarkedPdf {
    pub bytes: Vec<u8>,
    pub marks: u32,
}

/// The text of each page, in order. A form feed stands between pages in a
/// stored document, so one inside a page becomes a space.
pub(crate) fn page_texts(pdf: &[u8]) -> Result<Vec<String>, PdfError> {
    let pages = platform::page_texts(pdf)?;
    if pages.iter().all(|page| page.trim().is_empty()) {
        return Err(PdfError::Unreadable);
    }
    Ok(pages
        .into_iter()
        .map(|page| page.replace('\u{c}', " "))
        .collect())
}

pub(crate) fn marked_copy(pdf: &[u8], marks: &[PdfMark]) -> Result<MarkedPdf, PdfError> {
    let marks = serde_json::to_string(marks).map_err(|_| PdfError::WriteFailed)?;
    platform::marked_copy(pdf, &marks)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{MarkedPdf, PdfError};
    use std::ffi::{c_char, c_void, CStr, CString};

    unsafe extern "C" {
        fn sona_pdf_page_texts(bytes: *const u8, length: usize, out_json: *mut *mut c_char) -> i32;
        fn sona_pdf_marked_copy(
            bytes: *const u8,
            length: usize,
            marks_json: *const c_char,
            out_bytes: *mut *mut u8,
            out_length: *mut usize,
            out_marked: *mut u32,
        ) -> i32;
        fn sona_pdf_free(pointer: *mut c_void);
    }

    pub(super) fn page_texts(pdf: &[u8]) -> Result<Vec<String>, PdfError> {
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: the bridge reads `pdf.len()` bytes and writes one pointer into the local.
        let result = unsafe { sona_pdf_page_texts(pdf.as_ptr(), pdf.len(), &mut json) };
        if result != 0 || json.is_null() {
            return Err(PdfError::Unreadable);
        }
        // SAFETY: on success the bridge hands over a NUL-terminated string it
        // allocated; it is copied once here.
        let text = unsafe { CStr::from_ptr(json) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: json is the bridge's live allocation and nothing borrows it now.
        unsafe { sona_pdf_free(json.cast()) };
        serde_json::from_str(&text).map_err(|_| PdfError::Unreadable)
    }

    pub(super) fn marked_copy(pdf: &[u8], marks: &str) -> Result<MarkedPdf, PdfError> {
        let marks = CString::new(marks).map_err(|_| PdfError::WriteFailed)?;
        let mut bytes: *mut u8 = std::ptr::null_mut();
        let mut length = 0_usize;
        let mut marked = 0_u32;
        // SAFETY: the bridge reads the slice and marks, which outlive it, and writes the locals.
        let result = unsafe {
            sona_pdf_marked_copy(
                pdf.as_ptr(),
                pdf.len(),
                marks.as_ptr(),
                &mut bytes,
                &mut length,
                &mut marked,
            )
        };
        match result {
            0 if !bytes.is_null() => {}
            2 => return Err(PdfError::Unreadable),
            _ => return Err(PdfError::WriteFailed),
        }
        // SAFETY: on success the bridge hands over a `length`-byte buffer it
        // allocated; it is copied once and freed by the bridge.
        let copied = unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec();
        // SAFETY: bytes is the bridge's live allocation; the slice above is gone.
        unsafe { sona_pdf_free(bytes.cast()) };
        Ok(MarkedPdf {
            bytes: copied,
            marks: marked,
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{MarkedPdf, PdfError};

    pub(super) fn page_texts(_pdf: &[u8]) -> Result<Vec<String>, PdfError> {
        Err(PdfError::Unreadable)
    }

    pub(super) fn marked_copy(_pdf: &[u8], _marks: &str) -> Result<MarkedPdf, PdfError> {
        Err(PdfError::WriteFailed)
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    /// A PDF of `pages`, each a list of lines set in Helvetica from the top
    /// of a letter-size page, built byte by byte so its offsets are exact.
    pub(crate) fn pdf(pages: &[&[&str]]) -> Vec<u8> {
        let font = 3 + 2 * pages.len();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                (0..pages.len())
                    .map(|page| format!("{} 0 R", 3 + 2 * page))
                    .collect::<Vec<_>>()
                    .join(" "),
                pages.len()
            ),
        ];
        for (page, lines) in pages.iter().enumerate() {
            let shown: String = lines
                .iter()
                .map(|line| {
                    let escaped = line
                        .replace('\\', "\\\\")
                        .replace('(', "\\(")
                        .replace(')', "\\)");
                    format!("({escaped}) Tj T* ")
                })
                .collect();
            let content = format!("BT /F1 12 Tf 16 TL 72 720 Td {shown}ET");
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>",
                4 + 2 * page
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{fixture, marked_copy, page_texts, PdfError, PdfMark};

    #[test]
    fn a_pdf_reads_page_by_page_and_one_with_no_text_is_unreadable() {
        let pdf = fixture::pdf(&[
            &["Residential Lease", "The rent is $2,500 per month."],
            &[
                "Tenant shall give Landlord sixty (60) days notice",
                "before moving out.",
            ],
        ]);

        let pages = page_texts(&pdf).unwrap();

        assert_eq!(pages.len(), 2);
        assert!(pages[0].starts_with("Residential Lease"));
        assert!(pages[0].contains("$2,500 per month") && !pages[0].contains("notice"));
        assert!(pages[1].starts_with("Tenant shall give Landlord sixty (60) days notice"));
        assert_eq!(page_texts(&fixture::pdf(&[&[]])), Err(PdfError::Unreadable));
        assert_eq!(page_texts(b"not a pdf"), Err(PdfError::Unreadable));
    }

    #[test]
    fn a_marked_copy_keeps_every_page_and_holds_a_note_per_passage_it_found() {
        let pdf = fixture::pdf(&[
            &["Residential Lease", "The rent is $2,500 per month."],
            &[
                "Tenant shall give Landlord sixty (60) days notice",
                "before moving out.",
            ],
        ]);
        let pages = page_texts(&pdf).unwrap();
        let passage = "Tenant shall give Landlord sixty (60) days notice".to_string();

        let marked = marked_copy(
            &pdf,
            &[
                PdfMark {
                    page: 1,
                    passage: passage.clone(),
                    note: "Dana said “thirty days notice” at 0:01".to_string(),
                },
                PdfMark {
                    page: 0,
                    passage,
                    note: "Not on this page".to_string(),
                },
            ],
        )
        .unwrap();

        assert_eq!(marked.marks, 1);
        assert_ne!(marked.bytes, pdf);
        assert_eq!(page_texts(&marked.bytes).unwrap(), pages);
    }
}
