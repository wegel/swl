// SPDX-License-Identifier: GPL-3.0-only

//! Type-safe coordinate system wrappers to prevent coordinate space confusion.
//! 
//! We have several coordinate spaces in the compositor:
//! - Global: smithay's global coordinate space across all outputs
//! - OutputRelative: coordinates relative to a specific physical output (0,0 at output's top-left)
//! - VirtualOutputRelative: coordinates relative to a virtual output's logical rectangle
//! 
//! Using wrapper types prevents accidentally passing the wrong coordinate space to functions.

use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::desktop::space::SpaceElement;
use std::ops::{Add, Sub};

/// A point in smithay's global coordinate space
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlobalPoint(pub Point<i32, Logical>);

/// A point relative to a physical output's coordinate space (0,0 at output's top-left)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRelativePoint(pub Point<i32, Logical>);

/// A point relative to a virtual output's logical rectangle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualOutputRelativePoint(pub Point<i32, Logical>);

/// A floating-point position in the global coordinate space (for cursor positions)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalPointF64(pub Point<f64, Logical>);

/// A rectangle in smithay's global coordinate space
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalRect(pub Rectangle<i32, Logical>);

/// A rectangle relative to a physical output's coordinate space
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRelativeRect(pub Rectangle<i32, Logical>);

/// A rectangle relative to a virtual output's logical rectangle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualOutputRelativeRect(pub Rectangle<i32, Logical>);

impl GlobalPoint {
    pub fn new(x: i32, y: i32) -> Self {
        Self(Point::new(x, y))
    }
    
    /// Convert global coordinates to output-relative coordinates
    pub fn to_output_relative(self, output_position: GlobalPoint) -> OutputRelativePoint {
        OutputRelativePoint(self.0 - output_position.0)
    }
    
    /// Access the underlying Point
    pub fn as_point(&self) -> Point<i32, Logical> {
        self.0
    }
    
    /// Convert to f64 coordinates for calculations
    pub fn to_f64(&self) -> smithay::utils::Point<f64, Logical> {
        self.0.to_f64()
    }
}

impl OutputRelativePoint {
    pub fn new(x: i32, y: i32) -> Self {
        Self(Point::new(x, y))
    }
    
    /// Convert output-relative coordinates to global coordinates
    pub fn to_global(self, output_position: GlobalPoint) -> GlobalPoint {
        GlobalPoint(self.0 + output_position.0)
    }
    
    /// Offset by a delta
    pub fn offset_by(self, dx: i32, dy: i32) -> Self {
        Self(Point::new(self.0.x + dx, self.0.y + dy))
    }
    
    /// Access the underlying Point
    pub fn as_point(&self) -> Point<i32, Logical> {
        self.0
    }
}

impl VirtualOutputRelativePoint {
    pub fn new(x: i32, y: i32) -> Self {
        Self(Point::new(x, y))
    }
    
    /// Convert virtual-output-relative coordinates to global coordinates
    pub fn to_global(self, vout_global_position: GlobalPoint) -> GlobalPoint {
        GlobalPoint(self.0 + vout_global_position.0)
    }
    
    /// Convert virtual-output-relative coordinates to output-relative coordinates
    #[allow(dead_code)]
    pub fn to_output_relative(self, vout_global_position: GlobalPoint, output_position: GlobalPoint) -> OutputRelativePoint {
        let global = self.to_global(vout_global_position);
        global.to_output_relative(output_position)
    }
    
    /// Access the underlying Point
    pub fn as_point(&self) -> Point<i32, Logical> {
        self.0
    }
}

impl GlobalPointF64 {
    /// Create a new floating-point global point
    pub fn new(x: f64, y: f64) -> Self {
        Self(Point::from((x, y)))
    }
    
    /// Access the underlying Point
    pub fn as_point(&self) -> Point<f64, Logical> {
        self.0
    }
    
    /// Convert from window center (common case for cursor positioning)
    pub fn from_center(rect: Rectangle<i32, Logical>) -> Self {
        Self::new(
            rect.loc.x as f64 + rect.size.w as f64 / 2.0,
            rect.loc.y as f64 + rect.size.h as f64 / 2.0,
        )
    }
    
    /// Convert from global rectangle center
    #[allow(dead_code)]
    pub fn from_global_rect_center(rect: &GlobalRect) -> Self {
        Self::from_center(rect.0)
    }
}

impl From<(f64, f64)> for GlobalPointF64 {
    fn from((x, y): (f64, f64)) -> Self {
        Self::new(x, y)
    }
}

impl GlobalRect {
    pub fn new(loc: GlobalPoint, size: Size<i32, Logical>) -> Self {
        Self(Rectangle::new(loc.0, size))
    }
    
    /// Create from location and size
    pub fn from_loc_and_size(loc: GlobalPoint, size: Size<i32, Logical>) -> Self {
        Self::new(loc, size)
    }
    
    pub fn location(&self) -> GlobalPoint {
        GlobalPoint(self.0.loc)
    }
    
    pub fn size(&self) -> Size<i32, Logical> {
        self.0.size
    }
    
    pub fn as_rectangle(&self) -> Rectangle<i32, Logical> {
        self.0
    }
    
    /// Convert to f64 coordinates for calculations
    pub fn to_f64(&self) -> smithay::utils::Rectangle<f64, Logical> {
        self.0.to_f64()
    }
    
    /// Check if this rectangle contains a point
    pub fn contains(&self, point: impl Into<Point<i32, Logical>>) -> bool {
        self.0.contains(point)
    }
}

impl OutputRelativeRect {
    #[allow(dead_code)]
    pub fn new(loc: OutputRelativePoint, size: Size<i32, Logical>) -> Self {
        Self(Rectangle::new(loc.0, size))
    }
    
    #[allow(dead_code)]
    pub fn location(&self) -> OutputRelativePoint {
        OutputRelativePoint(self.0.loc)
    }
    
    #[allow(dead_code)]
    pub fn size(&self) -> Size<i32, Logical> {
        self.0.size
    }
    
    #[allow(dead_code)]
    pub fn as_rectangle(&self) -> Rectangle<i32, Logical> {
        self.0
    }
}

impl VirtualOutputRelativeRect {
    pub fn new(loc: VirtualOutputRelativePoint, size: Size<i32, Logical>) -> Self {
        Self(Rectangle::new(loc.0, size))
    }
    
    /// Create from location and size
    pub fn from_loc_and_size(loc: VirtualOutputRelativePoint, size: Size<i32, Logical>) -> Self {
        Self::new(loc, size)
    }
    
    /// Create with a y offset (useful for tab bars)
    pub fn with_y_offset(area: &Self, y_offset: i32) -> Self {
        Self(Rectangle::new(
            Point::new(area.0.loc.x, area.0.loc.y + y_offset),
            Size::new(area.0.size.w, area.0.size.h - y_offset),
        ))
    }
    
    pub fn location(&self) -> VirtualOutputRelativePoint {
        VirtualOutputRelativePoint(self.0.loc)
    }
    
    pub fn size(&self) -> Size<i32, Logical> {
        self.0.size
    }
    
    pub fn as_rectangle(&self) -> Rectangle<i32, Logical> {
        self.0
    }
}

// Arithmetic operations
impl Add<Point<i32, Logical>> for VirtualOutputRelativePoint {
    type Output = Self;
    
    fn add(self, rhs: Point<i32, Logical>) -> Self::Output {
        Self(self.0 + rhs)
    }
}

impl Sub<Point<i32, Logical>> for VirtualOutputRelativePoint {
    type Output = Self;
    
    fn sub(self, rhs: Point<i32, Logical>) -> Self::Output {
        Self(self.0 - rhs)
    }
}

impl Add<GlobalPoint> for Point<i32, Logical> {
    type Output = GlobalPoint;
    
    fn add(self, rhs: GlobalPoint) -> Self::Output {
        GlobalPoint(self + rhs.0)
    }
}

// Helper trait for easy conversion from smithay Output methods
pub trait OutputExt {
    fn current_location_typed(&self) -> GlobalPoint;
}

impl OutputExt for smithay::output::Output {
    fn current_location_typed(&self) -> GlobalPoint {
        GlobalPoint(self.current_location())
    }
}

// Helper trait for easy conversion from smithay Space methods  
pub trait SpaceExt<W> {
    fn element_location_typed(&self, element: &W) -> Option<GlobalPoint>;
}

impl<W> SpaceExt<W> for smithay::desktop::Space<W> 
where 
    W: SpaceElement + PartialEq,
{
    fn element_location_typed(&self, element: &W) -> Option<GlobalPoint> {
        self.element_location(element).map(GlobalPoint)
    }
}

// Conversions from smithay types
impl From<Point<i32, Logical>> for GlobalPoint {
    fn from(point: Point<i32, Logical>) -> Self {
        Self(point)
    }
}

impl From<Point<i32, Logical>> for OutputRelativePoint {
    fn from(point: Point<i32, Logical>) -> Self {
        Self(point)
    }
}

impl From<Point<i32, Logical>> for VirtualOutputRelativePoint {
    fn from(point: Point<i32, Logical>) -> Self {
        Self(point)
    }
}

impl From<Rectangle<i32, Logical>> for GlobalRect {
    fn from(rect: Rectangle<i32, Logical>) -> Self {
        Self(rect)
    }
}

impl From<Rectangle<i32, Logical>> for OutputRelativeRect {
    fn from(rect: Rectangle<i32, Logical>) -> Self {
        Self(rect)
    }
}

impl From<Rectangle<i32, Logical>> for VirtualOutputRelativeRect {
    fn from(rect: Rectangle<i32, Logical>) -> Self {
        Self(rect)
    }
}

impl From<GlobalRect> for VirtualOutputRelativeRect {
    fn from(rect: GlobalRect) -> Self {
        // convert global rect to virtual-output-relative by translating to origin
        let size = rect.size();
        Self(Rectangle::new(Point::new(0, 0), size))
    }
}

// Conversions back to smithay types
impl From<GlobalRect> for Rectangle<i32, Logical> {
    fn from(rect: GlobalRect) -> Self {
        rect.0
    }
}