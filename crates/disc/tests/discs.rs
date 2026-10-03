//! Reads the player's own disc images, named in `GRIM_DISC` (separated by the system's path
//! separator): every file of every language they carry must unpack and match its MD5.

use std::sync::Arc;

#[test]
fn every_language_of_the_disc_unpacks() {
    let Some(list) = std::env::var_os("GRIM_DISC") else {
        eprintln!("GRIM_DISC is not set: no disc image to check");
        return;
    };
    for image in std::env::split_paths(&list) {
        let disc = grim_disc::Disc::open(Arc::new(grim_disc::FileSource::open(&image).unwrap())).unwrap();
        for language in disc.languages.clone() {
            if language == "int" && disc.default_language().is_none() {
                continue;
            }
            let mut files = 0;
            disc.install(&language, &|_| true, &mut |_, _, _, _| {
                files += 1;
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{}, {language}: {e}", image.display()));
            assert!(files > 500, "{}, {language}: only {files} files", image.display());
        }
    }
}
