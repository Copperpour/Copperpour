//! Regression tests for the production EasyEDA 3D orchestration, with only
//! download and writer failure boundaries substituted where necessary.
use super::*;
use crate::sourcing::easyeda::Easyeda3dAssets;

const OBJ: &str = include_str!("../../tests/fixtures/easyeda-3d/placement.obj");
const STEP: &[u8] = include_bytes!("../../tests/fixtures/easyeda-3d/resistor.step");
const FOOTPRINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/easyeda-3d/SOT-23.kicad_mod"
);
fn metadata() -> serde_json::Value {
    // Purpose-written EasyEDA-format metadata with an off-canvas origin.
    json!({"head":{"x":4000.,"y":3000.,"c_para":{"package":"MODEL-PKG", "link":"https://example.com/test.pdf"}},
        "shape":[format!("SVGNODE~{}",json!({"attrs":{"c_etype":"outline3D","uuid":"fixture-model","c_rotation":"0,0,180","c_origin":"608,-320","z":"0"},
            "childNodes":[{"attrs":{"points":"3999 2999 4001 2999 4001 3001 3999 3001"}}]}))]})
}
fn shapes() -> Vec<serde_json::Value> {
    metadata()["shape"].as_array().unwrap().clone()
}
fn assets(step: bool) -> Easyeda3dAssets {
    Easyeda3dAssets {
        step: if step {
            Ok(STEP.to_vec())
        } else {
            Err("HTTP 404".into())
        },
        obj: Ok(OBJ.into()),
    }
}
fn footprint(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("SOT-23.kicad_mod");
    std::fs::copy(FOOTPRINT, &path).unwrap();
    path
}
fn ctx() -> ToolContext {
    super::tests::test_ctx()
}

#[test]
fn model_rotations_cover_x_y_and_composed_axes_with_independent_extrema() {
    // Right-handed 90-degree axis rotations: X maps (x,y,z) to (x,-z,y),
    // Y maps it to (z,y,-x); X then Y gives (y,-z,-x).
    let obj = "v 1 2 3\nv 3 6 9\n";
    for (rotation, expected_offset, expected_rotation) in [
        ([90., 0., 0.], [8., 26., -1.], [270., 0., 0.]),
        ([0., 90., 0.], [4., 16., 4.], [0., 270., 0.]),
        ([90., 90., 0.], [6., 26., 4.], [270., 270., 0.]),
    ] {
        let spec = Easyeda3dSpec {
            uuid: "test".into(),
            target_center_mm: [10., 20.],
            z_mm: 1.,
            c_rotation_deg: rotation,
        };
        assert_eq!(
            compute_kicad_3d_transform(&spec, Some(obj)),
            Some((expected_offset, expected_rotation))
        );
    }
}

#[test]
fn model_fixture_metadata_and_bounds_pin_placement() {
    let spec = parse_easyeda_3d_model(&shapes(), 4000., 3000.).unwrap();
    assert_eq!(spec.uuid, "fixture-model");
    // Hand-calculated rotated extrema: X [-12,-10], Y [-24,-20],
    // Z [-5,-1], so the offset must centre XY and put the bottom on Z=0.
    assert_eq!(
        compute_kicad_3d_transform(&spec, Some(OBJ)),
        Some(([11., 22., 5.], [0., 0., 180.]))
    );
    assert!(is_step_bytes(STEP));
    assert!(!is_step_bytes(b"ISO-10303-21;"));
}

#[test]
fn model_malformed_meshes_are_refused_without_changing_topology_or_panicking() {
    let vertices = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\n";
    for face in [
        "1 2 999 3 4",
        "1 0 2 3",
        "1 nope 2 3",
        "1 2",
        "-999 1 2 3",
        "-9223372036854775808 1 2 3",
    ] {
        assert!(
            obj_to_vrml(&format!("{vertices}f {face}\n")).is_none(),
            "{face}"
        );
    }
    assert!(obj_to_vrml(&format!("{vertices}v NaN 2 3\nf 1 2 3\n")).is_none());
    assert!(obj_bounds_rotated("v 1 nope 2 3\n", [0.; 3]).is_none());
    assert!(obj_to_vrml(&format!("{vertices}f -4 -3 -2 -1\n")).is_some());
}

#[tokio::test]
async fn model_production_orchestration_attaches_step_and_wrl_and_reuses_identical_files() {
    for use_step in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let fp = footprint(dir.path());
        let ctx = ctx();
        for _ in 0..2 {
            let result = attach_easyeda_3d_model_with(
                &shapes(),
                [4000., 3000.],
                dir.path(),
                "SOT-23",
                &fp,
                |_| async { Ok(assets(use_step)) },
                |args| {
                    let ctx = &ctx;
                    async move {
                        crate::tools::footprint_models::handle_set_footprint_models(&args, ctx)
                            .await
                    }
                },
            )
            .await
            .unwrap();
            assert_eq!(result["status"], "attached");
            assert_eq!(result["format"], if use_step { "step" } else { "wrl" });
            let path = PathBuf::from(result["path"].as_str().unwrap());
            let bytes = std::fs::read(&path).unwrap();
            if use_step {
                assert_eq!(bytes, STEP);
            } else {
                assert!(bytes.starts_with(b"#VRML V2.0 utf8"));
            }
            let observed = std::fs::read_to_string(&fp).unwrap();
            assert!(observed.contains(result["kicad_path"].as_str().unwrap()));
            assert!(observed.contains("(offset (xyz 11 22 5))"));
            assert!(observed.contains("(rotate (xyz 0 0 180))"));
            assert!(observed.contains("(pad \"1\""));
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("3dmodels"))
                .unwrap()
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn model_failures_before_publication_leave_footprint_and_destination_unchanged() {
    let cases = [
        Err(anyhow::anyhow!("download timeout")),
        Ok(Easyeda3dAssets {
            step: Ok(STEP.to_vec()),
            obj: Err("OBJ 503".into()),
        }),
        Ok(Easyeda3dAssets {
            step: Ok(b"ISO-10303-21;".to_vec()),
            obj: Ok("v 0 0 0\n".into()),
        }),
        Ok(Easyeda3dAssets {
            step: Err("STEP 404".into()),
            obj: Ok("v nope 0 0\n".into()),
        }),
    ];
    for download in cases {
        let dir = tempfile::tempdir().unwrap();
        let fp = footprint(dir.path());
        let before = std::fs::read(&fp).unwrap();
        let result = attach_easyeda_3d_model_with(
            &shapes(),
            [4000., 3000.],
            dir.path(),
            "SOT-23",
            &fp,
            |_| async { download },
            |_| async { panic!("must not call writer") },
        )
        .await
        .unwrap_err();
        assert!(result.is_error);
        assert_eq!(body_of(&result)["model_3d"]["status"], "error");
        assert_eq!(std::fs::read(&fp).unwrap(), before);
        assert!(!dir.path().join("3dmodels").exists());
    }
}

#[tokio::test]
async fn model_missing_metadata_does_not_download_or_write() {
    let dir = tempfile::tempdir().unwrap();
    let fp = footprint(dir.path());
    let report = attach_easyeda_3d_model_with(
        &[],
        [0.; 2],
        dir.path(),
        "SOT-23",
        &fp,
        |_| async { panic!("must not download") },
        |_| async { panic!("must not attach") },
    )
    .await
    .unwrap();
    assert_eq!(report["status"], "not_available");
    assert!(!dir.path().join("3dmodels").exists());
}

#[tokio::test]
async fn model_conflict_and_post_write_uncertainty_survive_import_response() {
    for uncertain in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let fp = footprint(dir.path());
        let ctx = ctx();
        let before = std::fs::read(&fp).unwrap();
        let result = attach_easyeda_3d_model_with(
            &shapes(),
            [4000., 3000.],
            dir.path(),
            "SOT-23",
            &fp,
            |_| async { Ok(assets(true)) },
            |args| {
                let ctx = &ctx;
                let fp = &fp;
                async move {
                    if uncertain {
                        let result =
                            crate::tools::footprint_models::handle_set_footprint_models(&args, ctx)
                                .await?;
                        assert!(!result.is_error);
                        // Inject the writer's typed readback failure after a real write.
                        Err(
                            copperpour_sexp::SexpError::ReadbackMismatch { path: fp.clone() }
                                .into(),
                        )
                    } else {
                        Ok(crate::tools::revision_conflict_result(
                            fp,
                            "test attachment",
                        ))
                    }
                }
            },
        )
        .await
        .unwrap_err();
        let final_result = import_3d_failure(result, &dir.path().join("parts.kicad_sym"), &fp);
        assert!(final_result.is_error);
        let body = body_of(&final_result);
        assert_eq!(
            body["error"]["kind"],
            if uncertain {
                "mutation_outcome_uncertain"
            } else {
                "conflict"
            }
        );
        assert_eq!(
            body["model_3d"]["status"],
            if uncertain { "uncertain" } else { "error" }
        );
        assert_eq!(body["success"], false);
        assert_eq!(body["imported"]["placement"], "not_attempted");
        assert_eq!(body["model_3d"]["retry_safe"], false);
        assert!(PathBuf::from(body["model_3d"]["path"].as_str().unwrap()).is_file());
        assert_eq!(std::fs::read(&fp).unwrap() == before, !uncertain);
    }
}

#[test]
fn model_publication_never_overwrites_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.wrl");
    publish_model(&path, b"old mesh").unwrap();
    publish_model(&path, b"old mesh").unwrap();
    let result = publish_model(&path, b"different mesh").unwrap_err();
    assert_eq!(body_of(&result)["error"]["kind"], "conflict");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old mesh");
}

#[tokio::test]
async fn model_full_import_reports_partial_failure_and_skips_requested_placement() {
    let dir = tempfile::tempdir().unwrap();
    let cache = crate::sourcing::easyeda::easyeda_cache_dir().unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let cache_file = cache.join("C954100001.json");
    let metadata = metadata();
    let bundle = json!({
        "title":"MODEL-TEST", "description":"Import failure regression",
        "dataStr":{"head":{"x":400.,"y":290.,"c_para":{"pre":"U?","name":"MODEL-TEST"}},
          "shape":["P~show~0~1~380~95~180~gge1~0^^380~95^^M380,95h10~#880000^^1~393.7~99~0~P1~start^^1~389.5~94~0~1~end"]},
        "packageDetail":{"dataStr":{"head":metadata["head"],
          "shape": ["PAD~RECT~4000~3000~1~2~1~~1~0~4000 3000~0",metadata["shape"][0]]}}
    });
    std::fs::write(&cache_file, serde_json::to_string(&bundle).unwrap()).unwrap();
    // A missing target proves placement wasn't attempted: its normal error must
    // not replace the model failure or claim the import succeeded.
    let schematic = dir.path().join("must-not-be-created.kicad_sch");
    let args = json!({"project_dir":dir.path(),"lcsc_id":"C954100001","include_3d":true,
        "schematic":schematic,"x":10.,"y":20.});
    let result = handle_import_jlcpcb_part_with_fetch(&args, &ctx(), |_| async {
        Err(anyhow::anyhow!("injected model download outage"))
    })
    .await;
    std::fs::remove_file(cache_file).unwrap();
    let result = result.unwrap();
    assert!(result.is_error);
    let body = body_of(&result);
    assert_eq!(body["success"], false);
    assert_eq!(body["model_3d"]["stage"], "download");
    assert_eq!(body["imported"]["placement"], "not_attempted");
    for field in ["symbol_path", "footprint_path"] {
        assert!(PathBuf::from(body["imported"][field].as_str().unwrap()).is_file());
    }
    assert!(!schematic.exists());
    assert!(!dir.path().join("3dmodels").exists());
}

#[tokio::test]
async fn model_part21_non_utf8_strings_are_preserved() {
    let bytes =
        b"ISO-10303-21;\nHEADER;/* legacy string: \xff */ENDSEC;\nDATA;ENDSEC;\nEND-ISO-10303-21;";
    assert!(is_step_bytes(bytes));
    let dir = tempfile::tempdir().unwrap();
    let fp = footprint(dir.path());
    let ctx = ctx();
    let report = attach_easyeda_3d_model_with(
        &shapes(),
        [4000., 3000.],
        dir.path(),
        "legacy",
        &fp,
        |_| async {
            Ok(Easyeda3dAssets {
                step: Ok(bytes.to_vec()),
                obj: Ok(OBJ.into()),
            })
        },
        |args| async move {
            crate::tools::footprint_models::handle_set_footprint_models(&args, &ctx).await
        },
    )
    .await
    .unwrap();
    assert_eq!(report["format"], "step");
    assert_eq!(
        std::fs::read(report["path"].as_str().unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn model_nonfinite_materials_are_refused_and_inline_comments_supported() {
    for material in ["Kd NaN 0 0", "Ks 0 inf 0", "d NaN"] {
        assert!(obj_to_vrml(&format!("newmtl body\n{material}\n{OBJ}")).is_none());
    }
    assert!(obj_to_vrml(&format!("{OBJ}f 1 2 3 # inline comment\n")).is_some());
}

#[cfg(unix)]
#[test]
fn model_publication_uses_normal_creation_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    // Ordinary creation supplies 0666 filtered by the process umask. Comparing
    // with it avoids changing the global umask while other tests are running.
    let control = dir.path().join("control");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&control)
        .unwrap();
    let expected = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;
    for name in ["model.step", "model.wrl"] {
        let path = dir.path().join(name);
        publish_model(&path, b"model bytes").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            expected
        );
        // Reusing identical assets must preserve an owner's later restrictions.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        publish_model(&path, b"model bytes").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
