//! Match native logical monitor coordinates to the compositor's EIS regions.
//! Never guess by enumeration order: that can click on the wrong monitor.

use crate::Screen;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 1.0
            && self.height >= 1.0
    }
}

#[derive(Debug, Clone)]
pub(super) struct PortalMonitor {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Monitor {
    pub name: String,
    pub native: Rect,
    portal: Rect,
    mapping_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Region {
    pub bounds: Rect,
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PointerMapping {
    pub region_index: usize,
    pub native: Rect,
    target: Rect,
}

impl PointerMapping {
    pub fn position(&self, x: i32, y: i32) -> (f32, f32) {
        let axis = |value: i32, start: f64, size: f64, target: f64, target_size: f64| {
            let ratio = ((f64::from(value) - start) / (size - 1.0).max(1.0)).clamp(0.0, 1.0);
            (target + ratio * (target_size - 1.0)) as f32
        };
        (
            axis(
                x,
                self.native.x,
                self.native.width,
                self.target.x,
                self.target.width,
            ),
            axis(
                y,
                self.native.y,
                self.native.height,
                self.target.y,
                self.target.height,
            ),
        )
    }
}

pub(super) fn match_monitors(
    screens: &[Screen],
    streams: &[PortalMonitor],
) -> Result<Vec<Monitor>, String> {
    if screens.is_empty() || streams.len() != screens.len() {
        return Err(
            "Select all local monitors in the Wayland sharing dialog, then restart input sharing."
                .into(),
        );
    }
    for (i, screen) in screens.iter().enumerate() {
        if screens[..i].iter().any(|other| {
            i64::from(screen.x) < i64::from(other.x) + i64::from(other.width)
                && i64::from(other.x) < i64::from(screen.x) + i64::from(screen.width)
                && i64::from(screen.y) < i64::from(other.y) + i64::from(other.height)
                && i64::from(other.y) < i64::from(screen.y) + i64::from(screen.height)
        }) {
            return Err("Mirrored or overlapping native monitors cannot be mapped safely for Wayland input.".into());
        }
    }
    let mut used = vec![false; streams.len()];
    let mut result = Vec::new();
    for screen in screens {
        let native = Rect {
            x: f64::from(screen.x),
            y: f64::from(screen.y),
            width: f64::from(screen.width),
            height: f64::from(screen.height),
        };
        if !native.valid() {
            return Err(format!(
                "Invalid native monitor geometry for {}.",
                screen.name
            ));
        }
        let mut candidates: Vec<usize> = streams
            .iter()
            .enumerate()
            .filter(|(i, stream)| !used[*i] && stream.position == Some((screen.x, screen.y)))
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() && screens.len() == 1 && streams.len() == 1 {
            candidates.push(0);
        }
        // Older portals may omit position. Only use size when it uniquely
        // identifies both the native monitor and the selected stream.
        if candidates.is_empty()
            && screens
                .iter()
                .filter(|s| s.width == screen.width && s.height == screen.height)
                .count()
                == 1
        {
            candidates = streams
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    !used[*i]
                        && s.position.is_none()
                        && s.size == Some((screen.width, screen.height))
                })
                .map(|(i, _)| i)
                .collect();
        }
        if candidates.len() != 1 {
            return Err(format!("Cannot unambiguously match Wayland monitor '{}'. Select all physical monitors; mirrored/ambiguous monitor layouts are not supported.", screen.name));
        }
        let index = candidates[0];
        used[index] = true;
        let stream = &streams[index];
        let (x, y) = stream.position.unwrap_or((screen.x, screen.y));
        let (width, height) = stream.size.unwrap_or((screen.width, screen.height));
        let portal = Rect {
            x: f64::from(x),
            y: f64::from(y),
            width: f64::from(width),
            height: f64::from(height),
        };
        if !portal.valid() {
            return Err(format!(
                "The portal returned invalid geometry for '{}'.",
                screen.name
            ));
        }
        result.push(Monitor {
            name: screen.name.clone(),
            native,
            portal,
            mapping_id: stream.mapping_id.clone().filter(|id| !id.is_empty()),
        });
    }
    Ok(result)
}

pub(super) fn bind_regions(
    monitors: &[Monitor],
    regions: &[Region],
) -> Result<Vec<PointerMapping>, String> {
    if monitors.is_empty() {
        return Err("No authorized Wayland monitors.".into());
    }
    let origin_x = monitors
        .iter()
        .map(|m| m.portal.x)
        .fold(f64::INFINITY, f64::min);
    let origin_y = monitors
        .iter()
        .map(|m| m.portal.y)
        .fold(f64::INFINITY, f64::min);
    let mut result = Vec::new();
    let mut used = vec![false; regions.len()];
    for monitor in monitors {
        // A strong mapping ID takes precedence over legacy geometry. Never
        // fall back through a conflicting ID, nor bind two monitors to one
        // region when a compositor supplies duplicate IDs.
        let mut candidates: Vec<_> = regions
            .iter()
            .enumerate()
            .filter(|(_, region)| {
                region.bounds.valid()
                    && monitor.mapping_id.is_some()
                    && region.mapping_id == monitor.mapping_id
            })
            .map(|(index, _)| index)
            .collect();
        if candidates.is_empty() {
            candidates = regions
                .iter()
                .enumerate()
                .filter(|(_, region)| {
                    region.bounds.valid()
                        && (monitor.mapping_id.is_none() || region.mapping_id.is_none())
                        && (region.bounds
                            == Rect {
                                x: monitor.portal.x - origin_x,
                                y: monitor.portal.y - origin_y,
                                ..monitor.portal
                            }
                            || (monitors.len() == 1 && regions.len() == 1))
                })
                .map(|(index, _)| index)
                .collect();
        }
        if candidates.len() != 1 || used[candidates[0]] {
            return Err(format!(
                "Waiting for an unambiguous EIS pointer region for '{}'.",
                monitor.name
            ));
        }
        used[candidates[0]] = true;
        result.push(PointerMapping {
            region_index: candidates[0],
            native: monitor.native,
            target: regions[candidates[0]].bounds,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn screen(name: &str, x: i32, y: i32, width: i32, height: i32) -> Screen {
        Screen {
            id: name.into(),
            device_id: "local".into(),
            name: name.into(),
            x,
            y,
            width,
            height,
            scale: 1.0,
            is_primary: x == 0 && y == 0,
        }
    }

    fn stream(x: i32, y: i32, width: i32, height: i32, id: &str) -> PortalMonitor {
        PortalMonitor {
            position: Some((x, y)),
            size: Some((width, height)),
            mapping_id: Some(id.into()),
        }
    }

    #[test]
    fn matches_monitor_ids_not_enumeration_order_and_handles_scaling() {
        let mut right = screen("right", 0, 0, 1920, 1080);
        right.scale = 1.5;
        let native = [screen("left", -1280, 0, 1280, 1024), right];
        let monitors = match_monitors(
            &native,
            &[
                stream(0, 0, 1920, 1080, "B"),
                stream(-1280, 0, 1280, 1024, "A"),
            ],
        )
        .unwrap();
        let regions = [
            Region {
                bounds: Rect {
                    x: 2560.0,
                    y: 0.0,
                    width: 3840.0,
                    height: 2160.0,
                },
                mapping_id: Some("B".into()),
            },
            Region {
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 2560.0,
                    height: 2048.0,
                },
                mapping_id: Some("A".into()),
            },
        ];
        let mappings = bind_regions(&monitors, &regions).unwrap();
        assert_eq!(mappings[0].region_index, 1);
        assert_eq!(mappings[0].position(-1280, 0), (0.0, 0.0));
        assert_eq!(mappings[0].position(-1, 1023), (2559.0, 2047.0));
        assert_eq!(mappings[1].position(1919, 1079), (6399.0, 2159.0));
    }

    #[test]
    fn legacy_regions_normalize_negative_desktop_origins() {
        let native = [
            screen("top", 0, -1200, 1920, 1200),
            screen("bottom", 0, 0, 1920, 1080),
        ];
        let streams = [
            stream(0, -1200, 1920, 1200, ""),
            stream(0, 0, 1920, 1080, ""),
        ];
        let monitors = match_monitors(&native, &streams).unwrap();
        let regions = [
            Region {
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1200.0,
                },
                mapping_id: None,
            },
            Region {
                bounds: Rect {
                    x: 0.0,
                    y: 1200.0,
                    width: 1920.0,
                    height: 1080.0,
                },
                mapping_id: None,
            },
        ];
        let mappings = bind_regions(&monitors, &regions).unwrap();
        assert_eq!(mappings[1].position(0, 0), (0.0, 1200.0));
    }

    #[test]
    fn partial_ambiguous_and_invalid_selections_are_rejected() {
        let native = [
            screen("one", 0, 0, 1920, 1080),
            screen("two", 1920, 0, 1920, 1080),
        ];
        assert!(match_monitors(&native, &[stream(0, 0, 1920, 1080, "A")]).is_err());
        let ambiguous = PortalMonitor {
            position: None,
            size: Some((1920, 1080)),
            mapping_id: None,
        };
        assert!(match_monitors(&native, &[ambiguous.clone(), ambiguous]).is_err());
        assert!(match_monitors(&native[..1], &[stream(0, 0, 0, 1080, "A")]).is_err());
        assert!(match_monitors(&[], &[]).is_err());
    }

    #[test]
    fn conflicting_region_id_never_falls_back_to_geometry() {
        let native = [screen("one", 0, 0, 10, 10)];
        let monitors = match_monitors(&native, &[stream(0, 0, 10, 10, "A")]).unwrap();
        let region = Region {
            bounds: monitors[0].native,
            mapping_id: Some("B".into()),
        };
        assert!(bind_regions(&monitors, &[region]).is_err());
    }

    #[test]
    fn mapping_clamps_edges_and_accepts_one_pixel_monitors() {
        let mapping = PointerMapping {
            region_index: 0,
            native: Rect {
                x: -1.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            target: Rect {
                x: 50.0,
                y: 25.0,
                width: 1.0,
                height: 1.0,
            },
        };
        assert_eq!(mapping.position(i32::MIN, i32::MAX), (50.0, 25.0));
        assert!(!mapping.native.contains(0.0, 0.0));
    }

    #[test]
    fn duplicate_ids_overlapping_monitors_and_extra_streams_are_rejected() {
        let native = [screen("one", 0, 0, 10, 10), screen("two", 10, 0, 10, 10)];
        let streams = [stream(0, 0, 10, 10, "same"), stream(10, 0, 10, 10, "same")];
        let monitors = match_monitors(&native, &streams).unwrap();
        let region = Region {
            bounds: monitors[0].native,
            mapping_id: Some("same".into()),
        };
        assert!(bind_regions(&monitors, &[region]).is_err());
        assert!(match_monitors(&native[..1], &streams).is_err());
        let overlapped = [screen("one", 0, 0, 10, 10), screen("two", 5, 0, 10, 10)];
        assert!(match_monitors(&overlapped, &streams).is_err());
    }

    #[test]
    fn explicit_mapping_id_wins_over_legacy_geometry() {
        let monitors =
            match_monitors(&[screen("one", 0, 0, 10, 10)], &[stream(0, 0, 10, 10, "A")]).unwrap();
        let regions = [
            Region {
                bounds: monitors[0].native,
                mapping_id: None,
            },
            Region {
                bounds: Rect {
                    x: 10.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                mapping_id: Some("A".into()),
            },
        ];
        assert_eq!(
            bind_regions(&monitors, &regions).unwrap()[0].region_index,
            1
        );
    }
}
