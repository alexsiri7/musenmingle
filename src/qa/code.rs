//! The code hash of a source, so a QA check runs after its scraper changes.
//!
//! Only the source's own file is hashed: shared helpers (`normalise.rs`,
//! `jsonld.rs`) change often and would re-check every source; the weekly
//! check covers them.

use sha2::{Digest, Sha256};

use crate::sources::Source;

/// Every file in `src/sources/` except `mod.rs` and `jsonld.rs`, by module
/// name (a test keeps this in step with the directory).
const SOURCE_FILES: &[(&str, &str)] = &[
    ("artlogic", include_str!("../sources/artlogic.rs")),
    ("barbican", include_str!("../sources/barbican.rs")),
    (
        "chisenhale_gallery",
        include_str!("../sources/chisenhale_gallery.rs"),
    ),
    (
        "clerkenwell_design_week",
        include_str!("../sources/clerkenwell_design_week.rs"),
    ),
    ("conway_hall", include_str!("../sources/conway_hall.rs")),
    ("courtauld", include_str!("../sources/courtauld.rs")),
    ("design_museum", include_str!("../sources/design_museum.rs")),
    (
        "foundling_museum",
        include_str!("../sources/foundling_museum.rs"),
    ),
    ("four_corners", include_str!("../sources/four_corners.rs")),
    ("garden_museum", include_str!("../sources/garden_museum.rs")),
    (
        "goldsmiths_cca",
        include_str!("../sources/goldsmiths_cca.rs"),
    ),
    (
        "handel_hendrix",
        include_str!("../sources/handel_hendrix.rs"),
    ),
    (
        "headstone_manor",
        include_str!("../sources/headstone_manor.rs"),
    ),
    (
        "horse_hospital",
        include_str!("../sources/horse_hospital.rs"),
    ),
    (
        "hunterian_museum",
        include_str!("../sources/hunterian_museum.rs"),
    ),
    ("ibraaz", include_str!("../sources/ibraaz.rs")),
    (
        "lisson_gallery",
        include_str!("../sources/lisson_gallery.rs"),
    ),
    ("luma", include_str!("../sources/luma.rs")),
    ("lux", include_str!("../sources/lux.rs")),
    (
        "mall_galleries",
        include_str!("../sources/mall_galleries.rs"),
    ),
    (
        "october_gallery",
        include_str!("../sources/october_gallery.rs"),
    ),
    (
        "old_royal_naval_college",
        include_str!("../sources/old_royal_naval_college.rs"),
    ),
    (
        "photographers_gallery",
        include_str!("../sources/photographers_gallery.rs"),
    ),
    (
        "royal_museums_greenwich",
        include_str!("../sources/royal_museums_greenwich.rs"),
    ),
    ("serpentine", include_str!("../sources/serpentine.rs")),
    ("soane_museum", include_str!("../sources/soane_museum.rs")),
    (
        "somerset_house",
        include_str!("../sources/somerset_house.rs"),
    ),
    ("tec", include_str!("../sources/tec.rs")),
    ("ticketmaster", include_str!("../sources/ticketmaster.rs")),
    (
        "two_temple_place",
        include_str!("../sources/two_temple_place.rs"),
    ),
    (
        "wellcome_collection",
        include_str!("../sources/wellcome_collection.rs"),
    ),
    (
        "whitechapel_gallery",
        include_str!("../sources/whitechapel_gallery.rs"),
    ),
    (
        "william_morris_gallery",
        include_str!("../sources/william_morris_gallery.rs"),
    ),
    (
        "william_morris_society",
        include_str!("../sources/william_morris_society.rs"),
    ),
];

/// The module segment of a source type path
/// (`musenmingle::sources::<module>::Type` → `<module>`).
pub fn module_of(type_path: &str) -> Option<&str> {
    let (_, rest) = type_path.split_once("::sources::")?;
    let (module, _) = rest.split_once("::")?;
    Some(module)
}

/// The first 16 hex characters of the SHA-256 of the source's file, or `""`
/// when it is not one of ours (then code changes are not noticed).
pub fn code_hash(source: &dyn Source) -> String {
    let Some(code) = module_of(source.type_path())
        .and_then(|m| SOURCE_FILES.iter().find(|(name, _)| *name == m))
        .map(|(_, code)| code)
    else {
        return String::new();
    };
    Sha256::digest(code.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lists_every_source_file() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sources");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter_map(|f| f.strip_suffix(".rs").map(str::to_string))
            .filter(|m| m != "mod" && m != "jsonld")
            .collect();
        files.sort();
        let mut table: Vec<String> = SOURCE_FILES.iter().map(|(m, _)| m.to_string()).collect();
        table.sort();
        assert_eq!(table, files, "update qa::code::SOURCE_FILES");
    }

    #[test]
    fn module_is_the_segment_after_sources() {
        assert_eq!(
            module_of("musenmingle::sources::barbican::Barbican"),
            Some("barbican")
        );
        assert_eq!(module_of("runner::FakeSource"), None);
    }

    #[test]
    fn unknown_modules_have_no_hash() {
        struct Fake;
        #[async_trait::async_trait]
        impl Source for Fake {
            fn key(&self) -> &str {
                "fake"
            }
            async fn fetch(
                &self,
                _: &crate::fetch::FetchContext,
            ) -> Result<Vec<crate::model::RawEvent>, crate::sources::SourceError> {
                Ok(Vec::new())
            }
            fn normalise(
                &self,
                _: &crate::model::RawEvent,
            ) -> Result<Option<crate::model::NewEvent>, crate::sources::SourceError> {
                Ok(None)
            }
        }
        assert_eq!(code_hash(&Fake), "");
    }
}
