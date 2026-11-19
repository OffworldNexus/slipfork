//! Selection of the transitional single buildable `L0` rootfs image.

use slipfork_edge::EdgeError;

use super::runtime_error;

/// The one buildable image the transitional single-rootfs boot materializes.
///
/// Per-service pods select their own image from `Forkfile`; until that boot path lands, the world
/// rootfs can only be materialized from exactly one `snow.yml` buildable Dockerfile.
pub(super) fn select_single_build_image(ids: &[String]) -> Result<usize, EdgeError> {
    match ids {
        [_] => Ok(0),
        [] => Err(runtime_error(
            "snow.yml declares no buildable Dockerfile under 'images:'; a project that cannot be built into an image is not a project Slipfork can fork\n  fix: add `images:` with one Dockerfile (id, context, dockerfile)",
        )),
        declared => Err(runtime_error(format!(
            "snow.yml declares more than one buildable image ({}); the single-rootfs world boots exactly one\n  fix: declare exactly one buildable Dockerfile image in snow.yml",
            declared.join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::select_single_build_image;

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn one_buildable_image_is_selected() {
        assert_eq!(
            select_single_build_image(&ids(&["api"])).expect("one image"),
            0
        );
    }

    #[test]
    fn zero_buildable_images_fail_closed_with_the_images_snippet() {
        let error = select_single_build_image(&[]).expect_err("no buildable image");
        let message = error.to_string();
        assert!(
            message.contains("declares no buildable Dockerfile"),
            "{message}"
        );
        assert!(message.contains("fix: add `images:`"), "{message}");
    }

    #[test]
    fn two_buildable_images_fail_closed_with_the_one_line_remedy() {
        let error = select_single_build_image(&ids(&["api", "worker"])).expect_err("two images");
        let message = error.to_string();
        assert!(
            message.contains("more than one buildable image (api, worker)"),
            "{message}"
        );
        assert!(
            message.contains("fix: declare exactly one buildable Dockerfile image in snow.yml"),
            "{message}"
        );
    }
}
