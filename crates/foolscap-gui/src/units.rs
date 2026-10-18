//! Lengths for the crop form: the unit choice and the margin checks.
//!
//! Kept apart from the widgets so the arithmetic can be tested without a
//! display.

/// A unit the user can type a length in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Points,
    Millimetres,
    Inches,
}

impl Unit {
    pub const ALL: [Unit; 3] = [Unit::Points, Unit::Millimetres, Unit::Inches];

    pub fn name(self) -> &'static str {
        match self {
            Unit::Points => "Points",
            Unit::Millimetres => "Millimetres",
            Unit::Inches => "Inches",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Unit::Points => "pt",
            Unit::Millimetres => "mm",
            Unit::Inches => "in",
        }
    }

    pub fn from_index(index: u32) -> Unit {
        Unit::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Unit::Points)
    }

    /// How many points make one of this unit.
    fn points_per_unit(self) -> f64 {
        match self {
            Unit::Points => 1.0,
            Unit::Millimetres => 72.0 / 25.4,
            Unit::Inches => 72.0,
        }
    }

    pub fn to_points(self, value: f64) -> f64 {
        value * self.points_per_unit()
    }

    pub fn points_in(self, points: f64) -> f64 {
        points / self.points_per_unit()
    }

    /// Digits to show after the decimal point.
    pub fn digits(self) -> u32 {
        match self {
            Unit::Points => 1,
            Unit::Millimetres => 1,
            Unit::Inches => 2,
        }
    }

    /// What one click of a spin button's arrow adds.
    pub fn step(self) -> f64 {
        match self {
            Unit::Points => 6.0,
            Unit::Millimetres => 1.0,
            Unit::Inches => 0.1,
        }
    }
}

/// Margins in points: top, bottom, left, right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarginValues {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// Check margins against a page of `page` points (width, height), when its
/// size is known. The message names the unit the user is typing in.
///
/// At least one margin must be above zero, or the crop would change nothing,
/// and the sides must leave a positive area.
pub fn check_margins(
    margins: &MarginValues,
    page: Option<(f64, f64)>,
    unit: Unit,
) -> Result<(), String> {
    let all = [margins.top, margins.bottom, margins.left, margins.right];
    if all.iter().any(|value| !value.is_finite() || *value < 0.0) {
        return Err("Margins cannot be negative.".to_string());
    }
    if all.iter().all(|value| *value == 0.0) {
        return Err(
            "Enter a margin on at least one side, or the crop changes nothing.".to_string(),
        );
    }
    if let Some((width, height)) = page {
        let shown = |points: f64| {
            format!(
                "{:.*} {}",
                unit.digits() as usize,
                unit.points_in(points),
                unit.short()
            )
        };
        if margins.left + margins.right >= width {
            return Err(format!(
                "Left and right add up to {}, but this page is only {} wide.",
                shown(margins.left + margins.right),
                shown(width)
            ));
        }
        if margins.top + margins.bottom >= height {
            return Err(format!(
                "Top and bottom add up to {}, but this page is only {} tall.",
                shown(margins.top + margins.bottom),
                shown(height)
            ));
        }
    }
    Ok(())
}

/// The part of the page that stays, as fractions of its width and height:
/// `(left, top, right, bottom)` measured from the top-left corner. Margins
/// that leave nothing are squeezed so a preview never draws a negative box.
pub fn kept_fractions(margins: &MarginValues, page: (f64, f64)) -> (f64, f64, f64, f64) {
    let (width, height) = page;
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0, 1.0, 1.0);
    }
    let left = (margins.left / width).clamp(0.0, 1.0);
    let right = (1.0 - margins.right / width).clamp(left, 1.0);
    let top = (margins.top / height).clamp(0.0, 1.0);
    let bottom = (1.0 - margins.bottom / height).clamp(top, 1.0);
    (left, top, right, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn margins(top: f64, bottom: f64, left: f64, right: f64) -> MarginValues {
        MarginValues {
            top,
            bottom,
            left,
            right,
        }
    }

    #[test]
    fn units_convert_to_points_and_back() {
        assert!((Unit::Inches.to_points(1.0) - 72.0).abs() < 1e-9);
        assert!((Unit::Millimetres.to_points(25.4) - 72.0).abs() < 1e-9);
        for unit in Unit::ALL {
            assert!((unit.points_in(unit.to_points(12.5)) - 12.5).abs() < 1e-9);
        }
        assert_eq!(Unit::from_index(1), Unit::Millimetres);
        assert_eq!(Unit::from_index(9), Unit::Points);
    }

    #[test]
    fn margins_must_leave_an_area() {
        let a4 = Some((595.0, 842.0));
        assert!(check_margins(&margins(36.0, 36.0, 36.0, 36.0), a4, Unit::Points).is_ok());

        let wide = check_margins(&margins(0.0, 0.0, 300.0, 295.0), a4, Unit::Points);
        assert!(wide.unwrap_err().contains("wide"));

        let tall = check_margins(&margins(500.0, 342.0, 0.0, 0.0), a4, Unit::Points);
        assert!(tall.unwrap_err().contains("tall"));
    }

    #[test]
    fn the_message_uses_the_chosen_unit() {
        let error = check_margins(
            &margins(0.0, 0.0, 144.0, 144.0),
            Some((288.0, 288.0)),
            Unit::Inches,
        )
        .unwrap_err();
        assert!(error.contains("4.00 in"), "{error}");
    }

    #[test]
    fn all_zero_and_negative_margins_are_refused() {
        assert!(check_margins(&margins(0.0, 0.0, 0.0, 0.0), None, Unit::Points).is_err());
        assert!(check_margins(&margins(-1.0, 0.0, 0.0, 0.0), None, Unit::Points).is_err());
        assert!(check_margins(&margins(f64::NAN, 0.0, 0.0, 0.0), None, Unit::Points).is_err());
    }

    #[test]
    fn without_a_page_size_only_the_basics_are_checked() {
        assert!(check_margins(&margins(1.0e6, 1.0e6, 0.0, 0.0), None, Unit::Points).is_ok());
    }

    #[test]
    fn the_preview_box_stays_inside_the_page() {
        let (left, top, right, bottom) =
            kept_fractions(&margins(100.0, 100.0, 50.0, 50.0), (500.0, 1000.0));
        assert!((left - 0.1).abs() < 1e-9 && (right - 0.9).abs() < 1e-9);
        assert!((top - 0.1).abs() < 1e-9 && (bottom - 0.9).abs() < 1e-9);

        let (left, _, right, _) = kept_fractions(&margins(0.0, 0.0, 400.0, 400.0), (500.0, 500.0));
        assert!(right >= left);
    }
}
