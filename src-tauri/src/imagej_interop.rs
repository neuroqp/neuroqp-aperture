//! Rust side of `scripts/check-imagej-interop.py`, which supplies the working
//! directory and step through the environment.
use crate::{annotation_io, roi, tiff_annotations};
use std::path::PathBuf;

#[test]
#[ignore = "run through scripts/check-imagej-interop.py"]
fn imagej_interop() {
    let directory = PathBuf::from(std::env::var_os("APERTURE_IMAGEJ_DIRECTORY").unwrap());
    if std::env::var("APERTURE_IMAGEJ_STEP").unwrap() == "reopen" {
        let annotations = tiff_annotations::read(&directory.join("imagej-edited.tif")).unwrap();
        let active = roi::decode(&annotations.active_roi.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(active.points.len(), 3);
        assert_eq!(active.points[2].x, 19.25);
        assert_eq!(active.points[2].y, 20.75);
        assert_eq!(active.points[2].counter, 99);
        assert_eq!(active.points[2].position, 0);
        return;
    }
    for name in ["gray16-stack.tif", "float32.tif", "rgb.tif", "plain.tif"] {
        let source = directory.join(name);
        let annotations = tiff_annotations::read(&source).unwrap();
        let mut active = match annotations.active_roi {
            Some(bytes) => {
                let active = roi::decode(&bytes).unwrap().unwrap();
                assert_eq!(active.points.len(), 2);
                assert_eq!(active.points[0].x, 12.25);
                assert_eq!(active.points[0].y, 16.75);
                assert_eq!(active.points[0].counter, 2);
                assert_eq!(active.points[0].position, 0);
                assert_eq!(active.point_type, 2);
                assert_eq!(active.size, 3);
                assert_eq!(active.color, Some(0xffff8000));
                assert!(active.show_labels);
                assert_eq!(roi::encode(&active).unwrap(), bytes);
                active
            }
            None => roi::PointRoi {
                points: vec![],
                point_type: 2,
                size: 3,
                color: Some(0xffff8000),
                show_labels: true,
                position: 0,
                channel: 0,
                slice: 0,
                frame: 0,
                source_index: None,
                overlay: false,
                raw: vec![],
            },
        };
        // Delete and insert while keeping imported metadata and the first counter.
        active.points.truncate(1);
        if active.points.is_empty() {
            active.points.push(roi::Point {
                x: 12.25,
                y: 16.75,
                counter: 2,
                position: 0,
            });
        }
        active.points.push(roi::Point {
            x: 7.125,
            y: 8.875,
            counter: 99,
            position: 0,
        });
        let overlays: Vec<_> = annotations
            .overlays
            .into_iter()
            .map(|bytes| match roi::decode(&bytes).unwrap() {
                Some(mut points) => {
                    assert_eq!(points.points[0].position, 2);
                    points.points[0].x = 5.25;
                    roi::encode(&points).unwrap()
                }
                None => bytes,
            })
            .collect();
        let encoded = roi::encode(&active).unwrap();
        assert!(tiff_annotations::save_copy(
            &source,
            &source,
            encoded.clone(),
            overlays.clone(),
            false
        )
        .is_err());
        let target = directory.join(format!("saved-{name}"));
        tiff_annotations::save_copy(&source, &target, encoded.clone(), overlays.clone(), true)
            .unwrap();
        let read = tiff_annotations::read(&target).unwrap();
        assert_eq!(read.active_roi.unwrap(), encoded);
        assert_eq!(read.overlays, overlays);
    }
    let source = directory.join("gray16-stack.tif");
    let basis = annotation_io::load_basis(&source).unwrap();
    let loaded = &basis.points;
    let mut cleared = loaded.clone();
    for selection in &mut cleared {
        selection.points.clear();
    }
    let cleared_target = directory.join("saved-cleared.tif");
    annotation_io::save(&source, &cleared_target, cleared, &basis, true).unwrap();
    assert!(annotation_io::load_basis(&cleared_target)
        .unwrap()
        .points
        .is_empty());
    let mut split = vec![];
    for (index, counter) in [(1, 2), (0, 4), (1, 4), (0, 2)] {
        let mut group = loaded[index].clone();
        group.points.retain(|p| p.counter == counter);
        if counter == 2 {
            group.point_type = 1;
            group.size = 6;
            group.color = Some(0xff00ff00);
            group.show_labels = false;
        }
        split.push(group);
    }
    let mut added = split[0].clone();
    added.source_index = None;
    added.points = vec![roi::Point {
        x: 21.5,
        y: 22.25,
        counter: 2,
        position: 0,
    }];
    split.push(added);
    let target = directory.join("saved-split.tif");
    annotation_io::save(&source, &target, split, &basis, true).unwrap();
    let reopened = annotation_io::load_basis(&target).unwrap();
    assert_eq!(reopened.points.len(), 5);
    annotation_io::save(
        &target,
        &directory.join("saved-split-reopened.tif"),
        reopened.points.clone(),
        &reopened,
        true,
    )
    .unwrap();
}
