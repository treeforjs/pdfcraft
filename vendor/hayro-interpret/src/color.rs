//! PDF colors and color spaces.

use crate::cache::{Cache, CacheKey};
use crate::function::Function;
use hayro_syntax::object;
use hayro_syntax::object::Array;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Name;
use hayro_syntax::object::Object;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
use moxcms::{
    ColorProfile, DataColorSpace, Layout, RenderingIntent, Transform8BitExecutor,
    TransformF32Executor, TransformOptions, Xyzd,
};
use smallvec::{SmallVec, ToSmallVec, smallvec};
use std::borrow::Cow;
use std::fmt::{Debug, Formatter};
use std::ops::Deref;
use std::cell::RefCell;
use std::sync::{Arc, LazyLock, OnceLock};

/// A storage for the components of colors.
pub type ColorComponents = SmallVec<[f32; 4]>;

/// An RGB color with an alpha channel.
#[derive(Debug, Copy, Clone)]
pub struct AlphaColor {
    components: [f32; 4],
}

impl AlphaColor {
    /// A black color.
    pub const BLACK: Self = Self::new([0., 0., 0., 1.]);

    /// A transparent color.
    pub const TRANSPARENT: Self = Self::new([0., 0., 0., 0.]);

    /// A white color.
    pub const WHITE: Self = Self::new([1., 1., 1., 1.]);

    /// Create a new color from the given components.
    pub const fn new(components: [f32; 4]) -> Self {
        Self { components }
    }

    /// Create a new color from RGB8 values.
    pub const fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        let components = [u8_to_f32(r), u8_to_f32(g), u8_to_f32(b), 1.];
        Self::new(components)
    }

    /// Return the color as premulitplied RGBF32.
    pub fn premultiplied(&self) -> [f32; 4] {
        [
            self.components[0] * self.components[3],
            self.components[1] * self.components[3],
            self.components[2] * self.components[3],
            self.components[3],
        ]
    }

    /// Create a new color from RGBA8 values.
    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        let components = [u8_to_f32(r), u8_to_f32(g), u8_to_f32(b), u8_to_f32(a)];
        Self::new(components)
    }

    /// Return the color as RGBA8.
    pub fn to_rgba8(&self) -> [u8; 4] {
        [
            (self.components[0] * 255.0 + 0.5) as u8,
            (self.components[1] * 255.0 + 0.5) as u8,
            (self.components[2] * 255.0 + 0.5) as u8,
            (self.components[3] * 255.0 + 0.5) as u8,
        ]
    }

    /// Return the components of the color as RGBF32.
    pub fn components(&self) -> [f32; 4] {
        self.components
    }
}

const fn u8_to_f32(x: u8) -> f32 {
    x as f32 * (1.0 / 255.0)
}

#[derive(Debug, Clone)]
pub(crate) enum ColorSpaceType {
    DeviceCmyk,
    DeviceGray,
    DeviceRgb,
    Pattern(ColorSpace),
    Indexed(Indexed),
    ICCBased(ICCProfile),
    CalGray(CalGray),
    CalRgb(CalRgb),
    Lab(Lab),
    Separation(Separation),
    DeviceN(DeviceN),
}

impl ColorSpaceType {
    fn new(object: Object<'_>, cache: &Cache) -> Option<Self> {
        Self::new_inner(object, cache)
    }

    fn new_inner(object: Object<'_>, cache: &Cache) -> Option<Self> {
        if let Object::Name(name) = object {
            return Self::new_from_name(&name);
        } else if let Object::Array(color_array) = object {
            let mut iter = color_array.flex_iter();
            let name = iter.next::<Name<'_>>()?;

            match name.deref() {
                ICC_BASED => {
                    let icc_stream = iter.next::<Stream<'_>>()?;
                    let dict = icc_stream.dict();
                    let num_components = dict.get::<usize>(N)?;

                    return cache.get_or_insert_with(icc_stream.cache_key(), || {
                        if let Some(decoded) = icc_stream.decoded().ok().as_ref() {
                            ICCProfile::new(decoded, num_components)
                                .map(|icc| {
                                    // TODO: For SVG and PNG we can assume that the output color space is
                                    // sRGB. If we ever implement PDF-to-PDF, we probably want to
                                    // let the user pass the native color type and don't make this optimization
                                    // if it's not sRGB.
                                    if icc.is_srgb() {
                                        Self::DeviceRgb
                                    } else {
                                        Self::ICCBased(icc)
                                    }
                                })
                                .or_else(|| {
                                    dict.get::<Object<'_>>(ALTERNATE)
                                        .and_then(|o| Self::new(o, cache))
                                })
                                .or_else(|| match dict.get::<u8>(N) {
                                    Some(1) => Some(Self::DeviceGray),
                                    Some(3) => Some(Self::DeviceRgb),
                                    Some(4) => Some(Self::DeviceCmyk),
                                    _ => None,
                                })
                        } else {
                            None
                        }
                    });
                }
                CALCMYK => return Some(Self::DeviceCmyk),
                CALGRAY => {
                    let cal_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::CalGray(CalGray::new(&cal_dict)?));
                }
                CALRGB => {
                    let cal_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::CalRgb(CalRgb::new(&cal_dict)?));
                }
                DEVICE_RGB | RGB => return Some(Self::DeviceRgb),
                DEVICE_GRAY | G => return Some(Self::DeviceGray),
                DEVICE_CMYK | CMYK => return Some(Self::DeviceCmyk),
                LAB => {
                    let lab_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::Lab(Lab::new(&lab_dict)?));
                }
                INDEXED | I => {
                    return Some(Self::Indexed(Indexed::new(&color_array, cache)?));
                }
                SEPARATION => {
                    return Some(Self::Separation(Separation::new(&color_array, cache)?));
                }
                DEVICE_N => {
                    return Some(Self::DeviceN(DeviceN::new(&color_array, cache)?));
                }
                PATTERN => {
                    let _ = iter.next::<Name<'_>>();
                    let cs = iter
                        .next::<Object<'_>>()
                        .and_then(|o| ColorSpace::new(o, cache))
                        .unwrap_or(ColorSpace::device_rgb());
                    return Some(Self::Pattern(cs));
                }
                _ => {
                    warn!("unsupported color space: {}", name.as_str());
                    return None;
                }
            }
        }

        None
    }

    fn new_from_name(name: &Name<'_>) -> Option<Self> {
        match name.deref() {
            DEVICE_RGB | RGB => Some(Self::DeviceRgb),
            DEVICE_GRAY | G => Some(Self::DeviceGray),
            DEVICE_CMYK | CMYK => Some(Self::DeviceCmyk),
            CALCMYK => Some(Self::DeviceCmyk),
            PATTERN => Some(Self::Pattern(ColorSpace::device_rgb())),
            _ => None,
        }
    }
}

/// A PDF color space.
#[derive(Debug, Clone)]
pub struct ColorSpace(Arc<ColorSpaceType>);

impl ColorSpace {
    /// Create a new color space from the given object.
    pub(crate) fn new(object: Object<'_>, cache: &Cache) -> Option<Self> {
        Some(Self(Arc::new(ColorSpaceType::new(object, cache)?)))
    }

    /// Create a new color space from the name.
    pub(crate) fn new_from_name(name: &Name<'_>) -> Option<Self> {
        ColorSpaceType::new_from_name(name).map(|c| Self(Arc::new(c)))
    }

    /// Return the device gray color space.
    pub(crate) fn device_gray() -> Self {
        Self(Arc::new(ColorSpaceType::DeviceGray))
    }

    /// Return the device RGB color space.
    pub(crate) fn device_rgb() -> Self {
        Self(Arc::new(ColorSpaceType::DeviceRgb))
    }

    /// Return the device CMYK color space.
    pub(crate) fn device_cmyk() -> Self {
        Self(Arc::new(ColorSpaceType::DeviceCmyk))
    }

    /// Return the pattern color space.
    pub(crate) fn pattern() -> Self {
        Self(Arc::new(ColorSpaceType::Pattern(Self::device_gray())))
    }

    pub(crate) fn pattern_cs(&self) -> Option<Self> {
        match self.0.as_ref() {
            ColorSpaceType::Pattern(cs) => Some(cs.clone()),
            _ => None,
        }
    }

    /// Return `true` if the current color space is the pattern color space.
    pub(crate) fn is_pattern(&self) -> bool {
        matches!(self.0.as_ref(), ColorSpaceType::Pattern(_))
    }

    /// Return `true` if the current color space is an indexed color space.
    pub(crate) fn is_indexed(&self) -> bool {
        matches!(self.0.as_ref(), ColorSpaceType::Indexed(_))
    }

    /// Get the default decode array for the color space.
    pub(crate) fn default_decode_arr(&self, n: f32) -> SmallVec<[(f32, f32); 4]> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)],
            ColorSpaceType::DeviceGray => smallvec![(0.0, 1.0)],
            ColorSpaceType::DeviceRgb => smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0)],
            ColorSpaceType::ICCBased(i) => smallvec![(0.0, 1.0); i.0.number_components],
            ColorSpaceType::CalGray(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::CalRgb(_) => smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0)],
            ColorSpaceType::Lab(l) => smallvec![
                (0.0, 100.0),
                (l.range[0], l.range[1]),
                (l.range[2], l.range[3]),
            ],
            ColorSpaceType::Indexed(_) => smallvec![(0.0, 2.0_f32.powf(n) - 1.0)],
            ColorSpaceType::Separation(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::DeviceN(d) => smallvec![(0.0, 1.0); d.num_components as usize],
            // Not a valid image color space.
            ColorSpaceType::Pattern(_) => smallvec![(0.0, 1.0)],
        }
    }

    pub(crate) fn inverted_default_decode_arr(&self, n: f32) -> SmallVec<[(f32, f32); 4]> {
        self.default_decode_arr(n)
            .iter()
            .map(|(min, max)| (*max, *min))
            .collect()
    }

    /// Get the initial color of the color space.
    pub(crate) fn initial_color(&self) -> ColorComponents {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => smallvec![0.0, 0.0, 0.0, 1.0],
            ColorSpaceType::DeviceGray => smallvec![0.0],
            ColorSpaceType::DeviceRgb => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::ICCBased(icc) => match icc.0.number_components {
                1 => smallvec![0.0],
                3 => smallvec![0.0, 0.0, 0.0],
                4 => smallvec![0.0, 0.0, 0.0, 1.0],
                _ => unreachable!(),
            },
            ColorSpaceType::CalGray(_) => smallvec![0.0],
            ColorSpaceType::CalRgb(_) => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::Lab(_) => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::Indexed(_) => smallvec![0.0],
            ColorSpaceType::Separation(_) => smallvec![1.0],
            ColorSpaceType::Pattern(c) => c.initial_color(),
            ColorSpaceType::DeviceN(d) => smallvec![1.0; d.num_components as usize],
        }
    }

    pub(crate) fn is_device_gray(&self) -> bool {
        matches!(self.0.as_ref(), ColorSpaceType::DeviceGray)
    }

    /// Get the number of components of the color space.
    pub(crate) fn num_components(&self) -> u8 {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => 4,
            ColorSpaceType::DeviceGray => 1,
            ColorSpaceType::DeviceRgb => 3,
            ColorSpaceType::ICCBased(icc) => icc.0.number_components as u8,
            ColorSpaceType::CalGray(_) => 1,
            ColorSpaceType::CalRgb(_) => 3,
            ColorSpaceType::Lab(_) => 3,
            ColorSpaceType::Indexed(_) => 1,
            ColorSpaceType::Separation(_) => 1,
            ColorSpaceType::Pattern(p) => p.num_components(),
            ColorSpaceType::DeviceN(d) => d.num_components,
        }
    }

    /// Turn the given component values and opacity into an RGBA color.
    pub fn to_rgba(&self, c: &[f32], opacity: f32, manual_scale: bool) -> AlphaColor {
        self.to_alpha_color(c, opacity, manual_scale)
            .unwrap_or(AlphaColor::BLACK)
    }
}

impl ToRgb for ColorSpace {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], manual_scale: bool) -> Option<()> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => {
                if input.len() == 4 {
                    let converted = [
                        f32_to_u8(input[0]),
                        f32_to_u8(input[1]),
                        f32_to_u8(input[2]),
                        f32_to_u8(input[3]),
                    ];
                    device_cmyk_u8(&converted, output)
                } else {
                    let converted = input.iter().copied().map(f32_to_u8).collect::<Vec<_>>();
                    device_cmyk_u8(&converted, output)
                }
            }
            ColorSpaceType::DeviceGray => {
                let converted = input.iter().copied().map(f32_to_u8).collect::<SmallVec<[u8; 4]>>();
                device_gray_u8(&converted, output)
            }
            ColorSpaceType::DeviceRgb => {
                let converted = input.iter().copied().map(f32_to_u8).collect::<SmallVec<[u8; 4]>>();
                device_rgb_u8(&converted, output)
            }
            ColorSpaceType::Pattern(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::Indexed(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::ICCBased(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::CalGray(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::CalRgb(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::Lab(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::Separation(i) => i.convert_f32(input, output, manual_scale),
            ColorSpaceType::DeviceN(i) => i.convert_f32(input, output, manual_scale),
        }
    }

    fn supports_u8(&self) -> bool {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => true,
            ColorSpaceType::DeviceGray => true,
            ColorSpaceType::DeviceRgb => true,
            ColorSpaceType::Pattern(i) => i.supports_u8(),
            ColorSpaceType::Indexed(i) => i.supports_u8(),
            ColorSpaceType::ICCBased(i) => i.supports_u8(),
            ColorSpaceType::CalGray(i) => i.supports_u8(),
            ColorSpaceType::CalRgb(i) => i.supports_u8(),
            ColorSpaceType::Lab(i) => i.supports_u8(),
            ColorSpaceType::Separation(i) => i.supports_u8(),
            ColorSpaceType::DeviceN(i) => i.supports_u8(),
        }
    }

    fn convert_u8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk => device_cmyk_u8(input, output),
            ColorSpaceType::DeviceGray => device_gray_u8(input, output),
            ColorSpaceType::DeviceRgb => device_rgb_u8(input, output),
            ColorSpaceType::Pattern(i) => i.convert_u8(input, output),
            ColorSpaceType::Indexed(i) => i.convert_u8(input, output),
            ColorSpaceType::ICCBased(i) => i.convert_u8(input, output),
            ColorSpaceType::CalGray(i) => i.convert_u8(input, output),
            ColorSpaceType::CalRgb(i) => i.convert_u8(input, output),
            ColorSpaceType::Lab(i) => i.convert_u8(input, output),
            ColorSpaceType::Separation(i) => i.convert_u8(input, output),
            ColorSpaceType::DeviceN(i) => i.convert_u8(input, output),
        }
    }

    fn is_none(&self) -> bool {
        match self.0.as_ref() {
            ColorSpaceType::Separation(s) => s.is_none(),
            ColorSpaceType::DeviceN(d) => d.is_none(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CalGray {
    white_point: [f32; 3],
    black_point: [f32; 3],
    gamma: f32,
}

// See <https://github.com/mozilla/pdf.js/blob/06f44916c8936b92f464d337fe3a0a6b2b78d5b4/src/core/colorspace.js#L752>
impl CalGray {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let white_point = dict.get::<[f32; 3]>(WHITE_POINT).unwrap_or([1.0, 1.0, 1.0]);
        let black_point = dict.get::<[f32; 3]>(BLACK_POINT).unwrap_or([0.0, 0.0, 0.0]);
        let gamma = dict.get::<f32>(GAMMA).unwrap_or(1.0);

        Some(Self {
            white_point,
            black_point,
            gamma,
        })
    }
}

impl ToRgb for CalGray {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        for (input, output) in input.iter().copied().zip(output.chunks_exact_mut(3)) {
            let g = self.gamma;
            let (_xw, yw, _zw) = {
                let wp = self.white_point;
                (wp[0], wp[1], wp[2])
            };
            let (_xb, _yb, _zb) = {
                let bp = self.black_point;
                (bp[0], bp[1], bp[2])
            };

            let a = input;
            let ag = a.powf(g);
            let l = yw * ag;
            let val = (0.0_f32.max(295.8 * l.powf(0.333_333_34) - 40.8) + 0.5) as u8;

            output.copy_from_slice(&[val, val, val]);
        }

        Some(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CalRgb {
    white_point: [f32; 3],
    black_point: [f32; 3],
    matrix: [f32; 9],
    gamma: [f32; 3],
}

// See <https://github.com/mozilla/pdf.js/blob/06f44916c8936b92f464d337fe3a0a6b2b78d5b4/src/core/colorspace.js#L846>
// Completely copied from there without really understanding the logic, but we get the same results as Firefox
// which should be good enough (and by viewing the `calrgb.pdf` test file in different viewers you will
// see that in many cases each viewer does whatever it wants, even PDF), so this is good enough for us.
impl CalRgb {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let white_point = dict.get::<[f32; 3]>(WHITE_POINT).unwrap_or([1.0, 1.0, 1.0]);
        let black_point = dict.get::<[f32; 3]>(BLACK_POINT).unwrap_or([0.0, 0.0, 0.0]);
        let matrix = dict
            .get::<[f32; 9]>(MATRIX)
            .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let gamma = dict.get::<[f32; 3]>(GAMMA).unwrap_or([1.0, 1.0, 1.0]);

        Some(Self {
            white_point,
            black_point,
            matrix,
            gamma,
        })
    }

    const BRADFORD_SCALE_MATRIX: [f32; 9] = [
        0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296,
    ];

    const BRADFORD_SCALE_INVERSE_MATRIX: [f32; 9] = [
        0.9869929, -0.1470543, 0.1599627, 0.4323053, 0.5183603, 0.0492912, -0.0085287, 0.0400428,
        0.9684867,
    ];

    const SRGB_D65_XYZ_TO_RGB_MATRIX: [f32; 9] = [
        3.2404542, -1.5371385, -0.4985314, -0.969_266, 1.8760108, 0.0415560, 0.0556434, -0.2040259,
        1.0572252,
    ];

    const FLAT_WHITEPOINT: [f32; 3] = [1.0, 1.0, 1.0];
    const D65_WHITEPOINT: [f32; 3] = [0.95047, 1.0, 1.08883];

    fn decode_l_constant() -> f32 {
        ((8.0_f32 + 16.0) / 116.0).powi(3) / 8.0
    }

    fn srgb_transfer_function(color: f32) -> f32 {
        if color <= 0.0031308 {
            (12.92 * color).clamp(0.0, 1.0)
        } else if color >= 0.99554525 {
            1.0
        } else {
            ((1.0 + 0.055) * color.powf(1.0 / 2.4) - 0.055).clamp(0.0, 1.0)
        }
    }

    fn matrix_product(a: &[f32; 9], b: &[f32; 3]) -> [f32; 3] {
        [
            a[0] * b[0] + a[1] * b[1] + a[2] * b[2],
            a[3] * b[0] + a[4] * b[1] + a[5] * b[2],
            a[6] * b[0] + a[7] * b[1] + a[8] * b[2],
        ]
    }

    fn to_flat(source_white_point: &[f32; 3], lms: &[f32; 3]) -> [f32; 3] {
        [
            lms[0] / source_white_point[0],
            lms[1] / source_white_point[1],
            lms[2] / source_white_point[2],
        ]
    }

    fn to_d65(source_white_point: &[f32; 3], lms: &[f32; 3]) -> [f32; 3] {
        [
            lms[0] * Self::D65_WHITEPOINT[0] / source_white_point[0],
            lms[1] * Self::D65_WHITEPOINT[1] / source_white_point[1],
            lms[2] * Self::D65_WHITEPOINT[2] / source_white_point[2],
        ]
    }

    fn decode_l(l: f32) -> f32 {
        if l < 0.0 {
            -Self::decode_l(-l)
        } else if l > 8.0 {
            ((l + 16.0) / 116.0).powi(3)
        } else {
            l * Self::decode_l_constant()
        }
    }

    fn compensate_black_point(source_bp: &[f32; 3], xyz_flat: &[f32; 3]) -> [f32; 3] {
        if source_bp == &[0.0, 0.0, 0.0] {
            return *xyz_flat;
        }

        let zero_decode_l = Self::decode_l(0.0);

        let mut out = [0.0; 3];
        for i in 0..3 {
            let src = Self::decode_l(source_bp[i]);
            let scale = (1.0 - zero_decode_l) / (1.0 - src);
            let offset = 1.0 - scale;
            out[i] = xyz_flat[i] * scale + offset;
        }

        out
    }

    fn normalize_white_point_to_flat(
        &self,
        source_white_point: &[f32; 3],
        xyz: &[f32; 3],
    ) -> [f32; 3] {
        if source_white_point[0] == 1.0 && source_white_point[2] == 1.0 {
            return *xyz;
        }
        let lms = Self::matrix_product(&Self::BRADFORD_SCALE_MATRIX, xyz);
        let lms_flat = Self::to_flat(source_white_point, &lms);
        Self::matrix_product(&Self::BRADFORD_SCALE_INVERSE_MATRIX, &lms_flat)
    }

    fn normalize_white_point_to_d65(
        &self,
        source_white_point: &[f32; 3],
        xyz: &[f32; 3],
    ) -> [f32; 3] {
        let lms = Self::matrix_product(&Self::BRADFORD_SCALE_MATRIX, xyz);
        let lms_d65 = Self::to_d65(source_white_point, &lms);
        Self::matrix_product(&Self::BRADFORD_SCALE_INVERSE_MATRIX, &lms_d65)
    }
}

impl ToRgb for CalRgb {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        for (input, output) in input.chunks_exact(3).zip(output.chunks_exact_mut(3)) {
            let input = [
                input[0].clamp(0.0, 1.0),
                input[1].clamp(0.0, 1.0),
                input[2].clamp(0.0, 1.0),
            ];

            let [r, g, b] = input;
            let [gr, gg, gb] = self.gamma;
            let [agr, bgg, cgb] = [
                if r == 1.0 { 1.0 } else { r.powf(gr) },
                if g == 1.0 { 1.0 } else { g.powf(gg) },
                if b == 1.0 { 1.0 } else { b.powf(gb) },
            ];

            let m = &self.matrix;
            let x = m[0] * agr + m[3] * bgg + m[6] * cgb;
            let y = m[1] * agr + m[4] * bgg + m[7] * cgb;
            let z = m[2] * agr + m[5] * bgg + m[8] * cgb;
            let xyz = [x, y, z];

            let xyz_flat = self.normalize_white_point_to_flat(&self.white_point, &xyz);
            let xyz_black = Self::compensate_black_point(&self.black_point, &xyz_flat);
            let xyz_d65 = self.normalize_white_point_to_d65(&Self::FLAT_WHITEPOINT, &xyz_black);
            let srgb_xyz = Self::matrix_product(&Self::SRGB_D65_XYZ_TO_RGB_MATRIX, &xyz_d65);

            output.copy_from_slice(&[
                (Self::srgb_transfer_function(srgb_xyz[0]) * 255.0 + 0.5) as u8,
                (Self::srgb_transfer_function(srgb_xyz[1]) * 255.0 + 0.5) as u8,
                (Self::srgb_transfer_function(srgb_xyz[2]) * 255.0 + 0.5) as u8,
            ]);
        }

        Some(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Lab {
    range: [f32; 4],
    profile: ICCProfile,
}

impl Lab {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let white_point = dict.get::<[f32; 3]>(WHITE_POINT).unwrap_or([1.0, 1.0, 1.0]);
        // Not sure how this should be used.
        let _black_point = dict.get::<[f32; 3]>(BLACK_POINT).unwrap_or([0.0, 0.0, 0.0]);
        let range = dict
            .get::<[f32; 4]>(RANGE)
            .unwrap_or([-100.0, 100.0, -100.0, 100.0]);

        let mut profile = ColorProfile::new_from_slice(include_bytes!("../assets/LAB.icc")).ok()?;
        profile.white_point = Xyzd::new(
            white_point[0] as f64,
            white_point[1] as f64,
            white_point[2] as f64,
        );

        let profile = ICCProfile::new_from_src_profile(
            profile, false,
            // This flag is only used to scale the values to [0.0, 1.0], but
            // we already take care of this in the `convert_f32` method.
            // Therefore, leave this as false, even though this is a LAB profile.
            false, 3,
        )?;

        Some(Self { range, profile })
    }
}

impl ToRgb for Lab {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], manual_scale: bool) -> Option<()> {
        if !manual_scale {
            // moxcms expects values between 0.0 and 1.0, so we need to undo
            // the scaling.

            let input = input
                .chunks_exact(3)
                .flat_map(|i| {
                    let l = i[0] / 100.0;
                    let a = (i[1] + 128.0) / 255.0;
                    let b = (i[2] + 128.0) / 255.0;

                    [l, a, b]
                })
                .collect::<Vec<_>>();

            self.profile.convert_f32(&input, output, manual_scale)
        } else {
            self.profile.convert_f32(input, output, manual_scale)
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Indexed {
    values: Vec<Vec<f32>>,
    hival: u8,
    base: Box<ColorSpace>,
}

impl Indexed {
    fn new(array: &Array<'_>, cache: &Cache) -> Option<Self> {
        let mut iter = array.flex_iter();
        // Skip name
        let _ = iter.next::<Name<'_>>()?;
        let base_color_space = ColorSpace::new(iter.next::<Object<'_>>()?, cache)?;
        let hival = iter.next::<u32>()?.min(u8::MAX as u32) as u8;

        let values = {
            let data = iter
                .next::<Stream<'_>>()
                .and_then(|s| s.decoded().ok())
                .or_else(|| {
                    iter.next::<object::String<'_>>()
                        .map(|s| Cow::Owned(s.to_vec()))
                })?;

            let num_components = base_color_space.num_components();

            let mut byte_iter = data.iter().copied();

            let mut vals = vec![];
            for _ in 0..=hival {
                let mut temp = vec![];

                for _ in 0..num_components {
                    temp.push(byte_iter.next()? as f32 / 255.0);
                }

                vals.push(temp);
            }

            vals
        };

        Some(Self {
            values,
            hival,
            base: Box::new(base_color_space),
        })
    }
}

impl ToRgb for Indexed {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        let mut indexed = vec![0.0; input.len() * self.base.num_components() as usize];

        for (input, output) in input
            .iter()
            .copied()
            .zip(indexed.chunks_exact_mut(self.base.num_components() as usize))
        {
            let idx = (input.clamp(0.0, self.hival as f32) + 0.5) as usize;
            output.copy_from_slice(&self.values[idx]);
        }

        self.base.convert_f32(&indexed, output, true)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Separation {
    alternate_space: ColorSpace,
    tint_transform: Function,
    is_none_separation: bool,
}

impl Separation {
    fn new(array: &Array<'_>, cache: &Cache) -> Option<Self> {
        let mut iter = array.flex_iter();
        // Skip `/Separation`
        let _ = iter.next::<Name<'_>>()?;
        let name = iter.next::<Name<'_>>()?;
        let alternate_space = ColorSpace::new(iter.next::<Object<'_>>()?, cache)?;
        let tint_transform = Function::new(&iter.next::<Object<'_>>()?)?;
        // Either I did something wrong, or no other viewers properly handles
        // `All`, so let's just ignore it as well.
        let is_none_separation = name.as_str() == "None";

        Some(Self {
            alternate_space,
            tint_transform,
            is_none_separation,
        })
    }
}

impl ToRgb for Separation {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        let evaluated = input
            .iter()
            .flat_map(|n| {
                self.tint_transform
                    .eval(smallvec![*n])
                    .unwrap_or(self.alternate_space.initial_color())
            })
            .collect::<Vec<_>>();
        self.alternate_space.convert_f32(&evaluated, output, false)
    }

    fn is_none(&self) -> bool {
        self.is_none_separation
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeviceN {
    alternate_space: ColorSpace,
    num_components: u8,
    tint_transform: Function,
    is_none: bool,
}

impl DeviceN {
    fn new(array: &Array<'_>, cache: &Cache) -> Option<Self> {
        let mut iter = array.flex_iter();
        // Skip `/DeviceN`
        let _ = iter.next::<Name<'_>>()?;
        // Skip `Name`.
        let names = iter
            .next::<Array<'_>>()?
            .iter::<Name<'_>>()
            .collect::<Vec<_>>();
        let num_components = u8::try_from(names.len()).ok()?;
        let all_none = names.iter().all(|n| n.as_str() == "None");
        let alternate_space = ColorSpace::new(iter.next::<Object<'_>>()?, cache)?;
        let tint_transform = Function::new(&iter.next::<Object<'_>>()?)?;

        if num_components == 0 {
            return None;
        }

        Some(Self {
            alternate_space,
            num_components,
            tint_transform,
            is_none: all_none,
        })
    }
}

impl ToRgb for DeviceN {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        let evaluated = input
            .chunks_exact(self.num_components as usize)
            .flat_map(|n| {
                self.tint_transform
                    .eval(n.to_smallvec())
                    .unwrap_or(self.alternate_space.initial_color())
            })
            .collect::<Vec<_>>();
        self.alternate_space.convert_f32(&evaluated, output, false)
    }

    fn is_none(&self) -> bool {
        self.is_none
    }
}

struct ICCColorRepr {
    src_profile: ColorProfile,
    src_layout: Layout,
    number_components: usize,
    is_srgb: bool,
    is_lab: bool,
    transform_u8: Arc<Transform8BitExecutor>,
    transform_f32: OnceLock<Option<Arc<TransformF32Executor>>>,
    /// PdfCraft patch (25): the rendering intent the transforms were made for.
    options: TransformOptions,
    /// PdfCraft patch (25): black point compensation, as one lookup table per output channel.
    bpc: Option<Box<[[u8; 256]; 3]>>,
}

#[derive(Clone)]
pub(crate) struct ICCProfile(Arc<ICCColorRepr>);

impl Debug for ICCProfile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ICCColor {{..}}")
    }
}

impl ICCProfile {
    fn new(profile: &[u8], number_components: usize) -> Option<Self> {
        let src_profile = ColorProfile::new_from_slice(profile).ok()?;

        const SRGB_MARKER: &[u8] = b"sRGB";

        let is_srgb = profile
            .get(52..56)
            .map(|device_model| device_model == SRGB_MARKER)
            .unwrap_or(false);
        let is_lab = src_profile.color_space == DataColorSpace::Lab;

        // PdfCraft patch (25): the colour settings' intent and black point compensation.
        let (intent, bpc) = current_device_color()
            .map(|d| (d.0.intent, d.0.black_point_compensation))
            .unwrap_or((RenderingIntent::Perceptual, false));
        Self::new_with_options(src_profile, is_srgb, is_lab, number_components, intent, bpc && !is_lab)
    }

    fn new_from_src_profile(
        src_profile: ColorProfile,
        is_srgb: bool,
        is_lab: bool,
        number_components: usize,
    ) -> Option<Self> {
        Self::new_with_options(
            src_profile,
            is_srgb,
            is_lab,
            number_components,
            RenderingIntent::Perceptual,
            false,
        )
    }

    /// PdfCraft patch (25): a profile converted to sRGB with `intent` (the profile's default
    /// table, then any other, when it has none for the intent), with black point compensation
    /// when `bpc` is set.
    fn new_with_options(
        src_profile: ColorProfile,
        is_srgb: bool,
        is_lab: bool,
        number_components: usize,
        intent: RenderingIntent,
        bpc: bool,
    ) -> Option<Self> {
        let src_layout = match number_components {
            1 => Layout::Gray,
            3 => Layout::Rgb,
            4 => Layout::Rgba,
            _ => {
                warn!("unsupported number of components {number_components} for ICC profile");

                return None;
            }
        };

        let dest_profile = ColorProfile::new_srgb();
        let mut chosen = None;
        for intent in [
            intent,
            RenderingIntent::Perceptual,
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation,
        ] {
            let options = TransformOptions {
                rendering_intent: intent,
                ..TransformOptions::default()
            };
            if let Ok(t) = src_profile.clone().create_transform_8bit(
                src_layout,
                &dest_profile,
                Layout::Rgb,
                options,
            ) {
                chosen = Some((t, options));
                break;
            }
        }
        let (transform_u8, options) = chosen?;
        let bpc = if bpc && !is_srgb {
            black_point_tables(&transform_u8, number_components)
        } else {
            None
        };

        Some(Self(Arc::new(ICCColorRepr {
            src_profile,
            src_layout,
            number_components,
            is_srgb,
            is_lab,
            transform_u8,
            transform_f32: OnceLock::new(),
            options,
            bpc,
        })))
    }

    fn is_srgb(&self) -> bool {
        self.0.is_srgb
    }

    fn is_lab(&self) -> bool {
        self.0.is_lab
    }

    fn transform_u8(&self) -> &Arc<Transform8BitExecutor> {
        &self.0.transform_u8
    }

    fn transform_f32(&self) -> Option<&Arc<TransformF32Executor>> {
        // From my benchmarking, creating the f32 transforms is usually much
        // more expensive than u8. Therefore, we only create it lazily when
        // really needed.
        // PdfCraft patch (25): with the intent the u8 transform was made with, and no panic
        // when the f32 transform can't be made.
        self.0
            .transform_f32
            .get_or_init(|| {
                let dest_profile = ColorProfile::new_srgb();
                self.0
                    .src_profile
                    .clone()
                    .create_transform_f32(
                        self.0.src_layout,
                        &dest_profile,
                        Layout::Rgb,
                        self.0.options,
                    )
                    .ok()
            })
            .as_ref()
    }

    /// PdfCraft patch (25): black point compensation on converted RGB triples.
    fn compensate(&self, output: &mut [u8]) {
        if let Some(tables) = &self.0.bpc {
            for px in output.chunks_exact_mut(3) {
                for (v, table) in px.iter_mut().zip(tables.iter()) {
                    *v = table[usize::from(*v)];
                }
            }
        }
    }
}

impl ToRgb for ICCProfile {
    fn convert_f32(&self, input: &[f32], output: &mut [u8], _: bool) -> Option<()> {
        let mut temp = vec![0.0_f32; output.len()];

        if self.is_lab() {
            // moxcms expects normalized values.
            let scaled = input
                .chunks_exact(3)
                .flat_map(|i| {
                    [
                        i[0] * (1.0 / 100.0),
                        (i[1] + 128.0) * (1.0 / 255.0),
                        (i[2] + 128.0) * (1.0 / 255.0),
                    ]
                })
                .collect::<Vec<_>>();
            self.transform_f32()?.transform(&scaled, &mut temp).ok()?;
        } else {
            self.transform_f32()?.transform(input, &mut temp).ok()?;
        };

        for (input, output) in temp.iter().zip(output.iter_mut()) {
            *output = (input * 255.0 + 0.5) as u8;
        }
        self.compensate(output);

        Some(())
    }

    fn supports_u8(&self) -> bool {
        true
    }

    fn convert_u8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if self.is_srgb() {
            // PdfCraft patch (25): mismatched lengths are refused rather than panicking.
            if input.len() != output.len() {
                return None;
            }
            output.copy_from_slice(input);
        } else {
            self.transform_u8().transform(input, output).ok()?;
            self.compensate(output);
        }

        Some(())
    }
}

#[inline(always)]
fn f32_to_u8(val: f32) -> u8 {
    (val * 255.0 + 0.5) as u8
}

#[derive(Debug, Clone)]
/// A color.
pub struct Color {
    color_space: ColorSpace,
    components: ColorComponents,
    opacity: f32,
}

impl Color {
    pub(crate) fn new(color_space: ColorSpace, components: ColorComponents, opacity: f32) -> Self {
        Self {
            color_space,
            components,
            opacity,
        }
    }

    /// Return the color as an RGBA color.
    pub fn to_rgba(&self) -> AlphaColor {
        self.color_space
            .to_rgba(&self.components, self.opacity, false)
    }

    /// Create a color from RGBA.
    pub fn from_rgba(rgba: AlphaColor) -> Self {
        let c = rgba.components();
        Self {
            color_space: ColorSpace::device_rgb(),
            components: smallvec![c[0], c[1], c[2]],
            opacity: c[3],
        }
    }
}

static CMYK_TRANSFORM: LazyLock<ICCProfile> = LazyLock::new(|| {
    ICCProfile::new(include_bytes!("../assets/CGATS001Compat-v2-micro.icc"), 4).unwrap()
});

/// PdfCraft patch (24): DeviceCMYK to RGB through the same CMYK profile pages render with.
/// `input` holds CMYK quadruples (0–255 each), `output` the RGB triples; `None` when the
/// lengths don't match or the conversion fails.
pub fn device_cmyk_to_rgb8(input: &[u8], output: &mut [u8]) -> Option<()> {
    if input.len() % 4 != 0 || output.len() != input.len() / 4 * 3 {
        return None;
    }
    CMYK_TRANSFORM.convert_u8(input, output)
}

/// PdfCraft patch (25): the bytes of the CMYK profile DeviceCMYK renders through by default.
pub fn default_cmyk_profile() -> &'static [u8] {
    include_bytes!("../assets/CGATS001Compat-v2-micro.icc")
}

/// PdfCraft patch (25): how DeviceCMYK becomes RGB under [`DeviceColor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmykConversion {
    /// Through the CMYK profile ([`DeviceColorSettings::cmyk`], else the default one).
    Profile,
    /// Without colour management: R = (1 − C)(1 − K), and so on.
    Naive,
}

/// PdfCraft patch (25): colour management settings for the device colour spaces, the input
/// of [`DeviceColor::new`].
#[derive(Clone, Debug)]
pub struct DeviceColorSettings {
    /// The profile DeviceGray is read in (a gray ICC profile); `None`: gray is sRGB gray.
    pub gray: Option<Arc<Vec<u8>>>,
    /// The profile DeviceRGB is read in (an RGB ICC profile); `None`: sRGB.
    pub rgb: Option<Arc<Vec<u8>>>,
    /// The profile DeviceCMYK is read in (a CMYK ICC profile); `None`: [`default_cmyk_profile`].
    pub cmyk: Option<Arc<Vec<u8>>>,
    /// Whether DeviceCMYK goes through a profile or the naive conversion.
    pub cmyk_conversion: CmykConversion,
    /// ICC rendering intent: 0 perceptual, 1 relative colorimetric, 2 saturation,
    /// 3 absolute colorimetric. A profile without a table for it uses its default table.
    pub intent: u8,
    /// Map the source black point to the display's black (on ICC profiles of the document
    /// too). Done on the converted sRGB values in linear light.
    pub black_point_compensation: bool,
}

struct DeviceColorRepr {
    gray: Option<ICCProfile>,
    rgb: Option<ICCProfile>,
    cmyk: Option<ICCProfile>,
    intent: RenderingIntent,
    black_point_compensation: bool,
}

/// PdfCraft patch (25): prepared colour management settings for the device colour spaces
/// ([`crate::InterpreterSettings::device_color`]): DeviceGray, DeviceRGB and DeviceCMYK are
/// converted to sRGB through the given profiles with the intent, and ICC-based colour of the
/// document with the intent and black point compensation.
#[derive(Clone)]
pub struct DeviceColor(Arc<DeviceColorRepr>);

impl Debug for DeviceColor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceColor {{ intent: {:?}, bpc: {} }}", self.0.intent, self.0.black_point_compensation)
    }
}

fn intent_from_u8(intent: u8) -> RenderingIntent {
    match intent {
        1 => RenderingIntent::RelativeColorimetric,
        2 => RenderingIntent::Saturation,
        3 => RenderingIntent::AbsoluteColorimetric,
        _ => RenderingIntent::Perceptual,
    }
}

impl DeviceColor {
    /// Prepare `settings`. An error names the profile that can't be used and why.
    pub fn new(settings: &DeviceColorSettings) -> Result<Self, String> {
        let intent = intent_from_u8(settings.intent);
        let bpc = settings.black_point_compensation;
        let load = |bytes: &[u8], n: usize, what: &str| -> Result<ICCProfile, String> {
            let profile = ColorProfile::new_from_slice(bytes)
                .map_err(|e| format!("the {what} profile can't be read: {e}"))?;
            let space_ok = matches!(
                (n, profile.color_space),
                (1, DataColorSpace::Gray) | (3, DataColorSpace::Rgb) | (4, DataColorSpace::Cmyk)
            );
            if !space_ok {
                return Err(format!("the {what} profile is not a {what} profile"));
            }
            let is_srgb = bytes.get(52..56).is_some_and(|m| m == b"sRGB");
            ICCProfile::new_with_options(profile, is_srgb, false, n, intent, bpc).ok_or_else(|| {
                format!("the {what} profile has no table that converts its colours to the display")
            })
        };
        let gray = settings.gray.as_deref().map(|b| load(b, 1, "gray")).transpose()?;
        let rgb = settings.rgb.as_deref().map(|b| load(b, 3, "RGB")).transpose()?;
        let cmyk = match settings.cmyk_conversion {
            CmykConversion::Naive => None,
            CmykConversion::Profile => Some(load(
                settings.cmyk.as_deref().map_or(default_cmyk_profile(), |b| b.as_slice()),
                4,
                "CMYK",
            )?),
        };
        Ok(Self(Arc::new(DeviceColorRepr {
            gray,
            rgb,
            cmyk,
            intent,
            black_point_compensation: bpc,
        })))
    }

    /// DeviceCMYK quadruples (0–255) to RGB triples as pages render them with these settings;
    /// `None` when the lengths don't match or the conversion fails.
    pub fn cmyk_to_rgb8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if input.len() % 4 != 0 || output.len() != input.len() / 4 * 3 {
            return None;
        }
        self.cmyk_u8(input, output)
    }

    /// DeviceRGB triples to sRGB triples with these settings.
    pub fn rgb_to_rgb8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if input.len() % 3 != 0 || output.len() != input.len() {
            return None;
        }
        self.rgb_u8(input, output)
    }

    /// DeviceGray values to sRGB triples with these settings.
    pub fn gray_to_rgb8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if output.len() != input.len().checked_mul(3)? {
            return None;
        }
        self.gray_u8(input, output)
    }

    fn cmyk_u8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match &self.0.cmyk {
            Some(p) => p.convert_u8(input, output),
            None => {
                for (q, o) in input.chunks_exact(4).zip(output.chunks_exact_mut(3)) {
                    let k = 255 - u16::from(q[3]);
                    for (c, v) in q.iter().take(3).zip(o.iter_mut()) {
                        *v = ((255 - u16::from(*c)) * k / 255) as u8;
                    }
                }
                Some(())
            }
        }
    }

    fn rgb_u8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match &self.0.rgb {
            Some(p) => p.convert_u8(input, output),
            None => identity_rgb(input, output),
        }
    }

    fn gray_u8(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match &self.0.gray {
            Some(p) => p.convert_u8(input, output),
            None => identity_gray(input, output),
        }
    }
}

thread_local! {
    /// PdfCraft patch (25): the device colour settings of the page being interpreted on this
    /// thread, set by [`crate::Context::new`].
    static DEVICE_COLOR: RefCell<Option<DeviceColor>> = const { RefCell::new(None) };
}

pub(crate) fn set_device_color(color: Option<DeviceColor>) {
    DEVICE_COLOR.with(|d| {
        if let Ok(mut d) = d.try_borrow_mut() {
            *d = color;
        }
    });
}

fn current_device_color() -> Option<DeviceColor> {
    DEVICE_COLOR.with(|d| d.try_borrow().ok().and_then(|d| d.clone()))
}

fn identity_rgb(input: &[u8], output: &mut [u8]) -> Option<()> {
    for (input, output) in input.iter().zip(output.iter_mut()) {
        *output = *input;
    }
    Some(())
}

fn identity_gray(input: &[u8], output: &mut [u8]) -> Option<()> {
    for (input, output) in input.iter().zip(output.chunks_exact_mut(3)) {
        output.copy_from_slice(&[*input, *input, *input]);
    }
    Some(())
}

fn device_cmyk_u8(input: &[u8], output: &mut [u8]) -> Option<()> {
    match current_device_color() {
        Some(d) => d.cmyk_u8(input, output),
        None => CMYK_TRANSFORM.convert_u8(input, output),
    }
}

fn device_rgb_u8(input: &[u8], output: &mut [u8]) -> Option<()> {
    match current_device_color() {
        Some(d) => d.rgb_u8(input, output),
        None => identity_rgb(input, output),
    }
}

fn device_gray_u8(input: &[u8], output: &mut [u8]) -> Option<()> {
    match current_device_color() {
        Some(d) => d.gray_u8(input, output),
        None => identity_gray(input, output),
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// PdfCraft patch (25): black point compensation tables: the sRGB value the profile gives its
/// darkest colour (no light for gray and RGB, all four inks for CMYK) is mapped to black, the
/// rest scaled in linear light so white stays white. `None` when the black is already black.
fn black_point_tables(
    transform: &Arc<Transform8BitExecutor>,
    number_components: usize,
) -> Option<Box<[[u8; 256]; 3]>> {
    let darkest: SmallVec<[u8; 4]> = match number_components {
        4 => smallvec![255, 255, 255, 255],
        n => smallvec![0; n],
    };
    let mut black = [0u8; 3];
    transform.transform(&darkest, &mut black).ok()?;
    let bp = black.map(|v| srgb_to_linear(f32::from(v) / 255.0));
    if bp.iter().all(|v| *v < 1e-4) {
        return None;
    }
    let mut tables = Box::new([[0u8; 256]; 3]);
    for (table, bp) in tables.iter_mut().zip(bp) {
        let span = (1.0 - bp).max(1e-3);
        for (i, out) in table.iter_mut().enumerate() {
            let lin = srgb_to_linear(i as f32 / 255.0);
            let v = ((lin - bp) / span).clamp(0.0, 1.0);
            *out = (linear_to_srgb(v) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    Some(tables)
}

pub(crate) trait ToRgb {
    fn convert_sample(&self, input: &[f32], output: &mut [u8], manual_scale: bool) -> Option<()> {
        // We prefer using the u8 variant for single samples, which is especially
        // important for ICC profiles to avoid constructing the (more expensive)
        // f32 variant.
        if self.supports_u8() {
            let converted = input
                .iter()
                .copied()
                .map(f32_to_u8)
                .collect::<SmallVec<[u8; 4]>>();

            if self.convert_u8(&converted, output).is_some() {
                return Some(());
            }
        }

        self.convert_f32(input, output, manual_scale)
    }

    fn convert_f32(&self, input: &[f32], output: &mut [u8], manual_scale: bool) -> Option<()>;
    fn supports_u8(&self) -> bool {
        false
    }
    fn convert_u8(&self, _: &[u8], _: &mut [u8]) -> Option<()> {
        unimplemented!();
    }
    fn is_none(&self) -> bool {
        false
    }
    fn to_alpha_color(
        &self,
        input: &[f32],
        mut opacity: f32,
        manual_scale: bool,
    ) -> Option<AlphaColor> {
        let mut output = [0; 3];
        self.convert_sample(input, &mut output, manual_scale)?;

        // For separation color spaces:
        // "The special colourant name None shall not produce any visible output.
        // Painting operations in a Separation space with this colourant name
        // shall have no effect on the current page."
        if self.is_none() {
            opacity = 0.0;
        }

        Some(AlphaColor::from_rgba8(
            output[0],
            output[1],
            output[2],
            (opacity * 255.0 + 0.5) as u8,
        ))
    }
}
