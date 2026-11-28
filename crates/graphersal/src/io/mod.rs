//! High-level I/O abstractions, facades, and format-specific importers/exporters.
//!
//! Two graph file formats: GraphSON 3.0 ([`graphson`], the primary exchange format for Gremlin
//! users) and GraphML ([`graphml`]). Neither carries the schema.

pub use crate::error::IOError;
use crate::graph::{Graph, GraphStorage};

/// GraphML import/export.
pub mod graphml;

/// GraphSON 3.0 import/export.
pub mod graphson;

pub use graphml::{GraphMLExport, GraphMLImport};
pub use graphson::{GraphSONExport, GraphSONImport, ImportNote, ImportNoteKind, ImportReport};

/// What saving a graph in a file format and loading that file into a new graph with the graph's
/// stored schema (set first, as the playground's "Load from file" does) would give, worked out
/// without writing or reading the file ([`GraphMLExport::reload_preview`],
/// [`GraphSONExport::reload_preview`]): each element goes through what the export writes and
/// what the import makes of it (the key types, the value text, the lifting of declared types,
/// the schema's checks), but no text is written and no second graph is built.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReloadPreview {
    /// The load succeeds; the schema inferred from the graph it builds (mode `none`), to compare
    /// with the original graph's inferred schema.
    Loads(crate::graph::schema::GraphSchema),
    /// The load fails with this error (the first element it fails on, in the file's order).
    Fails(IOError),
}

/// A new empty graph with `g`'s stored schema: the target a saved file is loaded into.
fn reload_target<S: GraphStorage>(g: &S) -> Result<crate::graph::TraversalGraph, IOError> {
    let mut target = crate::graph::TraversalGraph::new();
    if let Some(schema) = g.get_schema()? {
        target.set_schema(schema)?;
    }
    Ok(target)
}

/// Trait providing high-level graph import convenience methods.
///
/// # Examples
/// ```no_run
/// use graphersal::prelude::*;
///
/// let mut graph = GraphSource::empty();
/// graph.import_graphml("path/to/file.xml").unwrap();
/// let report = graph.import_graphson("path/to/file.json").unwrap();
/// println!("{report}");
/// ```
pub trait GraphImport {
    /// Imports GraphML formatted graph data from a file path.
    fn import_graphml(&mut self, path: &str) -> Result<(), IOError>;

    /// Imports GraphML formatted graph data from a buffered reader.
    fn import_graphml_reader<R: std::io::BufRead>(&mut self, reader: R) -> Result<(), IOError>;

    /// Imports GraphSON (line or wrapped form) from a file path, as one unit; returns what was
    /// imported, skipped and converted.
    fn import_graphson(&mut self, path: &str) -> Result<ImportReport, IOError>;

    /// Imports GraphSON from a buffered reader, as one unit (see [`Self::import_graphson`]).
    fn import_graphson_reader<R: std::io::BufRead>(
        &mut self,
        reader: R,
    ) -> Result<ImportReport, IOError>;
}

/// Trait providing high-level graph export convenience methods.
///
/// # Examples
/// ```no_run
/// use graphersal::prelude::*;
///
/// let graph = GraphSource::empty();
/// graph.export_graphml("path/to/file.xml").unwrap();
/// graph.export_graphson("path/to/file.json").unwrap();
/// ```
pub trait GraphExport {
    /// Exports graph data to a file path in GraphML format.
    fn export_graphml(&self, path: &str) -> Result<(), IOError>;

    /// Exports graph data to a writer stream in GraphML format.
    fn export_graphml_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError>;

    /// Exports graph data to a writer stream in GraphML format with options.
    fn export_graphml_writer_with_options<W: std::io::Write>(
        &self,
        writer: W,
        beautify: bool,
    ) -> Result<(), IOError> {
        let _ = beautify;
        self.export_graphml_writer(writer)
    }

    /// Exports graph data to a file path as GraphSON 3.0 (line form).
    fn export_graphson(&self, path: &str) -> Result<(), IOError>;

    /// Exports graph data to a writer stream as GraphSON 3.0 (line form).
    fn export_graphson_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError>;
}

/// Imports the GraphML file at `path` into the live graph `g` as one unit, under the process
/// memory ceiling.
pub(crate) fn import_graphml_file_with<S: GraphStorage>(
    g: &mut S,
    path: &str,
) -> Result<(), IOError> {
    let f = std::fs::File::open(path).map_err(|err| IOError::FileRead {
        path: path.to_string(),
        source: err,
    })?;
    let reader = quick_xml::Reader::from_reader(std::io::BufReader::with_capacity(128 * 1024, f));
    GraphMLImport {}.import_reader(g, reader)
}

/// Imports the GraphSON file at `path` into the live graph `g` as one unit, under the process
/// memory ceiling.
pub(crate) fn import_graphson_file_with<S: GraphStorage>(
    g: &mut S,
    path: &str,
) -> Result<ImportReport, IOError> {
    let f = std::fs::File::open(path).map_err(|err| IOError::FileRead {
        path: path.to_string(),
        source: err,
    })?;
    GraphSONImport::new().import_reader(g, std::io::BufReader::with_capacity(128 * 1024, f))
}

impl<S: GraphStorage> GraphImport for S {
    fn import_graphml(&mut self, path: &str) -> Result<(), IOError> {
        import_graphml_file_with(self, path)
    }

    fn import_graphml_reader<R: std::io::BufRead>(&mut self, reader: R) -> Result<(), IOError> {
        let importer = GraphMLImport {};
        let xml_reader = quick_xml::Reader::from_reader(reader);
        importer.import_reader(self, xml_reader)
    }

    fn import_graphson(&mut self, path: &str) -> Result<ImportReport, IOError> {
        import_graphson_file_with(self, path)
    }

    fn import_graphson_reader<R: std::io::BufRead>(
        &mut self,
        reader: R,
    ) -> Result<ImportReport, IOError> {
        GraphSONImport::new().import_reader(self, reader)
    }
}

impl GraphImport for Graph {
    fn import_graphml(&mut self, path: &str) -> Result<(), IOError> {
        self.write().import_graphml(path)
    }

    fn import_graphml_reader<R: std::io::BufRead>(&mut self, reader: R) -> Result<(), IOError> {
        self.write().import_graphml_reader(reader)
    }

    fn import_graphson(&mut self, path: &str) -> Result<ImportReport, IOError> {
        self.write().import_graphson(path)
    }

    fn import_graphson_reader<R: std::io::BufRead>(
        &mut self,
        reader: R,
    ) -> Result<ImportReport, IOError> {
        self.write().import_graphson_reader(reader)
    }
}

impl<S: GraphStorage> GraphExport for S {
    fn export_graphml(&self, path: &str) -> Result<(), IOError> {
        let exporter = GraphMLExport::new();
        let f = std::fs::File::create(path).map_err(|err| IOError::FileRead {
            path: path.to_string(),
            source: err,
        })?;
        let writer = std::io::BufWriter::with_capacity(128 * 1024, f);
        exporter.export_writer(self, writer)
    }

    fn export_graphml_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError> {
        let exporter = GraphMLExport::new();
        exporter.export_writer(self, writer)
    }

    fn export_graphml_writer_with_options<W: std::io::Write>(
        &self,
        writer: W,
        beautify: bool,
    ) -> Result<(), IOError> {
        let exporter = GraphMLExport::new().with_beautify(beautify);
        exporter.export_writer(self, writer)
    }

    fn export_graphson(&self, path: &str) -> Result<(), IOError> {
        GraphSONExport::new().export_file(self, path)
    }

    fn export_graphson_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError> {
        GraphSONExport::new().export_writer(self, writer)
    }
}

impl GraphExport for Graph {
    fn export_graphml(&self, path: &str) -> Result<(), IOError> {
        self.read().export_graphml(path)
    }

    fn export_graphml_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError> {
        self.read().export_graphml_writer(writer)
    }

    fn export_graphml_writer_with_options<W: std::io::Write>(
        &self,
        writer: W,
        beautify: bool,
    ) -> Result<(), IOError> {
        self.read()
            .export_graphml_writer_with_options(writer, beautify)
    }

    fn export_graphson(&self, path: &str) -> Result<(), IOError> {
        self.read().export_graphson(path)
    }

    fn export_graphson_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError> {
        self.read().export_graphson_writer(writer)
    }
}
