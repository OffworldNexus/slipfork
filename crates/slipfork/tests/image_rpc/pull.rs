//! A republished loopback tag: pull forgets and refreshes it, then a build pins the fresh digest.
use crate::common::{Lab, refs_beside};
use crate::registry::Registry;
use serde_json::{Value, json};
pub fn fresh_tag(lab: &Lab) {
    let registry = Registry::start();
    let reference = registry.reference("latest");
    lab.write_project(&reference);
    let params = json!({ "project": lab.project, "reference": reference });
    let (_, first) = lab.call("pull-old", "image.pull", &params);
    assert_eq!(first["exit"], 0, "{first}");
    let old = digest_for(&first, &reference);
    assert_eq!(
        old,
        registry.manifest_digest(),
        "resolve and pull observe one stable digest per tag version"
    );
    registry.publish();
    let (_, second) = lab.call("pull-fresh", "image.pull", &params);
    assert_eq!(second["exit"], 0, "{second}");
    let fresh = digest_for(&second, &reference);
    assert_ne!(fresh, old, "a republished tag re-resolves to a new digest");
    assert_eq!(
        fresh,
        registry.manifest_digest(),
        "the second pull remembers the republished digest"
    );
    let (_, built) = lab.call(
        "pull-build",
        "image.build",
        &json!({ "project": lab.project, "component": "app", "target": null, "no_cache": false }),
    );
    assert_eq!(built["exit"], 0, "{built}");
    assert_eq!(
        built["digest"].as_str(),
        Some(fresh.as_str()),
        "a warm build pins the pull: {built}"
    );
}
fn digest_for(reply: &Value, reference: &str) -> String {
    let rootfs = reply["rootfs"]
        .as_str()
        .expect("the pull reports its rootfs");
    refs_beside(rootfs)
        .remove(reference)
        .expect("refs.json records the pulled reference")
}
