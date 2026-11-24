//! A CSV file as rows of cells under their header, read the way every
//! tracker's export is: a byte-order mark dropped, headers matched without
//! regard to case or spacing, short rows padded.

use csv::ReaderBuilder;

pub(crate) struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn key(header: &str) -> String {
    header
        .trim()
        .trim_start_matches('\u{feff}')
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Table {
    pub(crate) fn read(text: &str) -> Result<Self, String> {
        let text = text.trim_start_matches('\u{feff}');
        let mut reader = ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::None)
            .from_reader(text.as_bytes());
        let headers = reader
            .headers()
            .map_err(|_| "The file is not a CSV file.".to_owned())?
            .iter()
            .map(key)
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|error| {
                let line = error.position().map_or(0, csv::Position::line);
                format!("Line {line} of the CSV file could not be read.")
            })?;
            if record.iter().all(|cell| cell.trim().is_empty()) {
                continue;
            }
            let mut row: Vec<String> = record.iter().map(str::to_owned).collect();
            row.resize(headers.len(), String::new());
            rows.push(row);
        }
        Ok(Self { headers, rows })
    }

    /// Whether every one of `names` is a column.
    pub(crate) fn has(&self, names: &[&str]) -> bool {
        names.iter().all(|name| self.column(name).is_some())
    }

    pub(crate) fn column(&self, name: &str) -> Option<usize> {
        let name = key(name);
        self.headers.iter().position(|header| *header == name)
    }

    /// The first of `names` that is a column.
    pub(crate) fn first_column(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|name| self.column(name))
    }

    pub(crate) fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows
            .iter()
            .map(move |cells| Row { table: self, cells })
    }

    /// Every column holding `name` in its header, in order, as a tracker
    /// repeats a column for each comment or label.
    pub(crate) fn columns_named(&self, name: &str) -> Vec<usize> {
        let name = key(name);
        self.headers
            .iter()
            .enumerate()
            .filter(|(_, header)| **header == name)
            .map(|(place, _)| place)
            .collect()
    }

    pub(crate) fn headers(&self) -> &[String] {
        &self.headers
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Row<'a> {
    table: &'a Table,
    cells: &'a [String],
}

impl<'a> Row<'a> {
    /// The trimmed cell under the first of `names` the table has, or none
    /// when the column is missing or the cell blank.
    pub(crate) fn get(&self, names: &[&str]) -> Option<&'a str> {
        let column = self.table.first_column(names)?;
        self.at(column)
    }

    pub(crate) fn at(&self, column: usize) -> Option<&'a str> {
        let cell = self.cells.get(column)?.trim();
        (!cell.is_empty()).then_some(cell)
    }

    pub(crate) fn text(&self, names: &[&str]) -> Option<String> {
        self.get(names).map(str::to_owned)
    }
}
