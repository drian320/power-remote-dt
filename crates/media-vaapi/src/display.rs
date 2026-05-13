//! VAAPI display open + capability probe.

use crate::error::VaapiError;
use std::path::{Path, PathBuf};

/// Scan `/dev/dri/renderD*` and return all candidate render nodes in
/// numerical order. Empty Vec = no render nodes (no GPU).
pub fn enumerate_render_nodes() -> Vec<PathBuf> {
    let dri = Path::new("/dev/dri");
    let Ok(rd) = std::fs::read_dir(dri) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    out.sort();
    out
}

/// Returns true when the system has at least one render node AND opening
/// it succeeds AND it advertises H264ConstrainedBaseline EncSlice.
/// Cached per-process via OnceLock.
pub fn vaapi_runtime_present() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| probe_first_capable_node().is_ok())
}

/// Walk render nodes and return the first one that supports
/// H264ConstrainedBaseline + EncSlice. Returns NoRenderNode if none
/// are usable.
pub fn probe_first_capable_node() -> Result<PathBuf, VaapiError> {
    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return Err(VaapiError::NoRenderNode);
    }
    for node in nodes {
        if node_supports_h264_baseline_encode(&node).unwrap_or(false) {
            return Ok(node);
        }
    }
    Err(VaapiError::NotSupported(
        "no render node advertises H264 EncSlice".into(),
    ))
}

fn node_supports_h264_baseline_encode(node: &Path) -> Result<bool, VaapiError> {
    // TODO(T5 implementer): use cros-libva's Display + query_config_profiles +
    // query_config_entrypoints to confirm the node supports
    // VAProfileH264ConstrainedBaseline + VAEntrypointEncSlice.
    //
    // The exact cros-libva API to invoke depends on what Step 1 probe
    // reveals; pseudocode:
    //
    //   let display = libva::Display::open(Some(node))?;
    //   let profiles = display.query_config_profiles()?;
    //   if !profiles.contains(&VAProfileH264ConstrainedBaseline) {
    //       return Ok(false);
    //   }
    //   let entrypoints = display.query_config_entrypoints(VAProfile...)?;
    //   Ok(entrypoints.contains(&VAEntrypointEncSlice))
    let _ = node;
    Ok(false) // P5C-1: be conservative; T7 wires the real probe.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_empty_when_no_dri_directory() {
        // In the container `/dev/dri` doesn't exist; smoke test that the
        // function tolerates this and returns Vec::new() instead of
        // panicking.
        let nodes = enumerate_render_nodes();
        assert!(nodes.is_empty() || nodes.iter().all(|p| p.starts_with("/dev/dri")));
    }

    #[test]
    fn vaapi_runtime_present_is_false_in_container() {
        // Container has no /dev/dri/* — expect false.
        assert!(!vaapi_runtime_present());
    }

    #[test]
    fn probe_returns_no_render_node_when_dir_empty() {
        // Same as above; we test the API shape.
        let r = probe_first_capable_node();
        assert!(matches!(
            r,
            Err(VaapiError::NoRenderNode) | Err(VaapiError::NotSupported(_))
        ));
    }
}
