//! Guards on built-ins a hostile script could use to take the viewer down.
//!
//! boa sizes some allocations straight from script values and loops natively over array
//! lengths, where its loop limit doesn't reach: `'ab'.repeat(2 ** 30)` asked for a 16 GiB
//! buffer (an allocation failure aborts the process), `Array.prototype.fill` on an array of
//! length 2^32 - 1 grew dense storage until memory ran out, and `f.apply(null, {length: 2 **
//! 30})` built a billion-entry argument list. Strings are capped in the vendored `boa_string`
//! (64 Mi code units); these wrappers refuse the rest with a `RangeError` before boa starts,
//! and otherwise call the original built-in.

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsFunction;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};

/// The longest array (or array-like) the guarded `Array` methods work on.
pub const MAX_ARRAY_LENGTH: u64 = 1 << 24;
/// The most arguments `apply`, `Reflect.apply` and `Reflect.construct` pass.
pub const MAX_ARGUMENTS: u64 = 1 << 16;
/// The longest string `repeat`, `padStart` and `padEnd` make (the vendored boa_string limit).
pub const MAX_STRING_LENGTH: u64 = 1 << 26;

/// `Array.prototype` methods that walk or fill `this` up to its length.
const ARRAY_METHODS: [&str; 26] = [
    "fill",
    "join",
    "toString",
    "toLocaleString",
    "sort",
    "toSorted",
    "reverse",
    "toReversed",
    "map",
    "forEach",
    "filter",
    "reduce",
    "reduceRight",
    "every",
    "some",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "indexOf",
    "lastIndexOf",
    "includes",
    "copyWithin",
    "flat",
    "flatMap",
    "with",
];

fn range(msg: &str) -> boa_engine::JsError {
    JsNativeError::range().with_message(msg.to_string()).into()
}

/// The `length` of `v` as an array-like (0 for values that aren't objects).
fn length_of(v: &JsValue, ctx: &mut Context) -> JsResult<u64> {
    match v.as_object() {
        Some(o) => o.get(js_string!("length"), ctx)?.to_length(ctx),
        None => Ok(0),
    }
}

/// `this` is too long for an `Array` method?
fn this_length(this: &JsValue, args: &[JsValue], original: &JsFunction, ctx: &mut Context) -> JsResult<JsValue> {
    if !this.is_null_or_undefined() {
        let o = this.to_object(ctx)?;
        if o.get(js_string!("length"), ctx)?.to_length(ctx)? > MAX_ARRAY_LENGTH {
            return Err(range("the array is too long for this script"));
        }
    }
    original.call(this, args, ctx)
}

/// `Array.from(arrayLike)` with too long an array-like?
fn from_length(this: &JsValue, args: &[JsValue], original: &JsFunction, ctx: &mut Context) -> JsResult<JsValue> {
    let source = args.first().cloned().unwrap_or_default();
    // Iterables (strings, arrays, sets) are bounded by their own size; a plain array-like isn't.
    if source.as_object().is_some() && length_of(&source, ctx)? > MAX_ARRAY_LENGTH {
        return Err(range("the array is too long for this script"));
    }
    original.call(this, args, ctx)
}

/// The argument list at `index` too long?
fn argument_list(index: usize) -> impl Fn(&JsValue, &[JsValue], &JsFunction, &mut Context) -> JsResult<JsValue> + Copy {
    move |this, args, original, ctx| {
        if let Some(list) = args.get(index)
            && length_of(list, ctx)? > MAX_ARGUMENTS
        {
            return Err(range("too many arguments"));
        }
        original.call(this, args, ctx)
    }
}

/// `String.prototype.repeat`, `padStart` and `padEnd` asked for too long a string?
fn string_length(this: &JsValue, args: &[JsValue], original: &JsFunction, ctx: &mut Context, repeat: bool) -> JsResult<JsValue> {
    if this.is_null_or_undefined() {
        return original.call(this, args, ctx);
    }
    let n = args.first().cloned().unwrap_or_default().to_number(ctx)?;
    if n.is_finite() && n > 0.0 {
        let len = this.to_string(ctx)?.len() as f64;
        let wanted = if repeat { len * n.trunc() } else { n.trunc() };
        if wanted > MAX_STRING_LENGTH as f64 {
            return Err(range("the string would be too long"));
        }
    }
    original.call(this, args, ctx)
}

/// Replace `object[name]` (a function) with `guard` around it.
fn wrap(
    ctx: &mut Context,
    object: &JsObject,
    name: &str,
    guard: impl Fn(&JsValue, &[JsValue], &JsFunction, &mut Context) -> JsResult<JsValue> + Copy + 'static,
) -> JsResult<()> {
    let key = JsString::from(name);
    let Some(original) = object.get(key.clone(), ctx)?.as_object().and_then(JsFunction::from_object) else { return Ok(()) };
    let length = original.get(js_string!("length"), ctx)?.to_length(ctx).unwrap_or(0);
    let f = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_copy_closure_with_captures(guard, original))
        .name(key.clone())
        .length(usize::try_from(length).unwrap_or(0))
        .build();
    object.define_property_or_throw(key, PropertyDescriptor::builder().value(f).writable(true).enumerable(false).configurable(true), ctx)?;
    Ok(())
}

/// Put the guards in place (once per engine, before any script runs).
pub(crate) fn harden(ctx: &mut Context) -> JsResult<()> {
    let array = ctx.intrinsics().constructors().array().prototype();
    for m in ARRAY_METHODS {
        wrap(ctx, &array, m, this_length)?;
    }
    let array_ctor = ctx.intrinsics().constructors().array().constructor();
    wrap(ctx, &array_ctor, "from", from_length)?;
    let function = ctx.intrinsics().constructors().function().prototype();
    wrap(ctx, &function, "apply", argument_list(1))?;
    if let Some(reflect) = ctx.global_object().get(js_string!("Reflect"), ctx)?.as_object() {
        wrap(ctx, &reflect, "apply", argument_list(2))?;
        wrap(ctx, &reflect, "construct", argument_list(1))?;
    }
    let string = ctx.intrinsics().constructors().string().prototype();
    wrap(ctx, &string, "repeat", |t, a, o, c| string_length(t, a, o, c, true))?;
    wrap(ctx, &string, "padStart", |t, a, o, c| string_length(t, a, o, c, false))?;
    wrap(ctx, &string, "padEnd", |t, a, o, c| string_length(t, a, o, c, false))?;
    Ok(())
}
