// Copyright (C) 2021  David Hoppenbrouwers
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::bytecode::CallResult;
use crate::{CallError, Environment, ScriptObject};
use core::cmp;
use core::fmt;
use core::ops::{Add, BitAnd, BitOr, BitXor, Div, Mul, Rem, Shl, Shr, Sub};
use std::rc::Rc;

#[derive(Clone)]
/// Variant type that encodes a few common types. Having the common types
/// not be hidden behind a dyn trait improves performance greatly
pub enum Variant {
	None,
	Bool(bool),
	Real(f64),
	Integer(isize),
	Char(char),
	String(Rc<str>),
	Object(ScriptObject),
}

impl Default for Variant {
	fn default() -> Self {
		Variant::None
	}
}

macro_rules! check_arg_count {
	($args:ident, $count:expr) => {
		if $args.len() != $count {
			return Err(CallError::BadArgumentCount);
		}
	};
}

macro_rules! gen_op {
	(
		$trait:ident, $fn:ident
		[$left:ident, $right:ident]
		$([$lhs:ident, $rhs:ident] => $out:ident $code:block)*
	) => {
		impl $trait<Self> for &Variant {
			type Output = CallResult;

			#[inline]
			fn $fn(self, rhs: Self) -> Self::Output {
				Ok(match (self, rhs) {
					$((Variant::$lhs($left), Variant::$rhs($right)) => Variant::$out($code),)*
					_ => return Err(CallError::IncompatibleType),
				})
			}
		}
	};
}

gen_op!(
	Add, add
	[rhs, lhs]
	[Real, Real] => Real { rhs + lhs }
	[Real, Integer] => Real { rhs + *lhs as f64 }
	[Integer, Real] => Real { *rhs as f64 + lhs }
	[Integer, Integer] => Integer { rhs + lhs }
	[String, String] => String {
		let mut out = rhs.to_string();
		out.extend(lhs.chars());
		out.into()
	}
);

gen_op!(
	Sub, sub
	[rhs, lhs]
	[Real, Real] => Real { rhs - lhs }
	[Real, Integer] => Real { rhs - *lhs as f64 }
	[Integer, Integer] => Integer { rhs - lhs }
);

gen_op!(
	Mul, mul
	[rhs, lhs]
	[Real, Real] => Real { rhs * lhs }
	[Real, Integer] => Real { rhs * *lhs as f64 }
	[Integer, Real] => Real { *rhs as f64 * lhs }
	[Integer, Integer] => Integer { rhs * lhs }
);

gen_op!(
	Div, div
	[rhs, lhs]
	[Real, Real] => Real { rhs / lhs }
	[Real, Integer] => Real { rhs / *lhs as f64 }
	[Integer, Real] => Real { *rhs as f64 / lhs }
	[Integer, Integer] => Integer { rhs / lhs }
);

gen_op!(
	Rem, rem
	[rhs, lhs]
	[Real, Real] => Real { rhs % lhs }
	[Real, Integer] => Real { rhs % *lhs as f64 }
	[Integer, Real] => Real { *rhs as f64 % lhs }
	[Integer, Integer] => Integer { rhs % lhs }
);

gen_op!(
	BitAnd, bitand
	[rhs, lhs]
	[Bool, Bool] => Bool { rhs & lhs }
	[Integer, Integer] => Integer { rhs & lhs }
);

gen_op!(
	BitOr, bitor
	[rhs, lhs]
	[Bool, Bool] => Bool { rhs | lhs }
	[Integer, Integer] => Integer { rhs | lhs }
);

gen_op!(
	BitXor, bitxor
	[rhs, lhs]
	[Bool, Bool] => Bool { rhs ^ lhs }
	[Integer, Integer] => Integer { rhs ^ lhs }
);

gen_op!(
	Shl, shl
	[rhs, lhs]
	[Integer, Integer] => Integer { rhs << lhs }
);

gen_op!(
	Shr, shr
	[rhs, lhs]
	[Integer, Integer] => Integer { rhs >> lhs }
);

impl PartialEq<Self> for Variant {
	#[inline]
	// FIXME should we return bool or should we implement a custom form
	// of PartialEq that returns a Result?
	fn eq(&self, rhs: &Self) -> bool {
		use Variant::*;
		match (self, rhs) {
			(Bool(a), Bool(b)) => a == b,
			(Real(a), Real(b)) => a == b,
			(Real(a), Integer(b)) => *a == *b as f64,
			(Integer(a), Real(b)) => *a as f64 == *b,
			(Integer(a), Integer(b)) => a == b,
			(String(a), String(b)) => a == b,
			(Char(a), Char(b)) => a == b,
			_ => false,
		}
	}
}

impl PartialOrd<Self> for Variant {
	#[inline]
	// FIXME ditto, pretty much
	fn partial_cmp(&self, rhs: &Self) -> Option<cmp::Ordering> {
		use Variant::*;
		match (self, rhs) {
			(Bool(a), Bool(b)) => a.partial_cmp(b),
			(Real(a), Real(b)) => a.partial_cmp(b),
			(Real(a), Integer(b)) => a.partial_cmp(&(*b as f64)),
			(Integer(a), Real(b)) => (*a as f64).partial_cmp(b),
			(Integer(a), Integer(b)) => a.partial_cmp(b),
			(String(a), String(b)) => a.partial_cmp(b),
			(Char(a), Char(b)) => a.partial_cmp(b),
			_ => Option::None,
		}
	}
}

macro_rules! call_tbl {
	{
		$var:ident
		$(
			[$variant:ident]
			$(
				$func:ident [$arg_count:expr] => $code:block
			)*
		)*
	} => {
		impl Variant {
			#[allow(unused_variables)]
			pub fn call(&self, function: &str, args: &[Variant], env: &Environment) -> CallResult {
				match self {
					Variant::None => Err(CallError::IsEmpty),
					$(
						Variant::$variant($var) => match function {
							$(
								stringify!($func) => {
									check_arg_count!(args, $arg_count);
									Ok($code)
								}
							)*
							_ => Err(CallError::UndefinedFunction),
						}
					)*
				}
			}
		}
	};
}

call_tbl! {
	var
	[Bool]
	[Real]
	abs [0] => { Variant::Real(var.abs()) }
	sqrt [0] => { Variant::Real(var.sqrt()) }
	[Integer]
	abs [0] => { Variant::Integer(var.abs()) }
	[Char]
	[String]
	len [0] => { Variant::Integer(var.len() as isize) }
	[Object]
}

impl Variant {
	pub fn iter(&self) -> CallResult<Box<dyn Iterator<Item = Variant>>> {
		match self {
			Variant::None => Err(CallError::IsEmpty),
			&Variant::Integer(i) => {
				if i < 0 {
					Ok(Box::new((-i + 1..=0).rev().map(|i| Variant::Integer(i))))
				} else {
					Ok(Box::new((0..i).map(|i| Variant::Integer(i))))
				}
			}
			Variant::String(s) => Ok(Box::new(StringIter::new(s.clone()))),
			Variant::Object(o) => o.iter(),
			_ => Err(CallError::IncompatibleType),
		}
	}

	pub fn index(&self, index: &Variant) -> CallResult {
		match self {
			Self::Object(obj) => obj.index(index),
			_ => Err(CallError::IncompatibleType),
		}
	}

	pub fn set_index(&self, index: &Variant, value: Variant) -> CallResult<()> {
		match self {
			Self::Object(obj) => obj.set_index(index, value),
			_ => Err(CallError::IncompatibleType),
		}
	}
}

impl fmt::Debug for Variant {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Variant::None => f.write_str("none"),
			Variant::Bool(false) => f.write_str("false"),
			Variant::Bool(true) => f.write_str("true"),
			Variant::Real(n) => write!(f, "{:?}", n),
			Variant::Integer(n) => write!(f, "{:?}", n),
			Variant::Char(n) => write!(f, "{:?}", n),
			Variant::String(n) => write!(f, "{:?}", n),
			//Variant::Object(n) => write!(f, "{}", n),
			Variant::Object(n) => f.write_str(n.to_string().as_str()),
		}
	}
}

impl fmt::Display for Variant {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Variant::None => f.write_str("none"),
			Variant::Bool(false) => f.write_str("false"),
			Variant::Bool(true) => f.write_str("true"),
			Variant::Real(n) => write!(f, "{}", n),
			Variant::Integer(n) => write!(f, "{}", n),
			Variant::Char(n) => write!(f, "{}", n),
			Variant::String(n) => f.write_str(n),
			//Variant::Object(n) => write!(f, "{}", n),
			Variant::Object(n) => f.write_str(n.to_string().as_str()),
		}
	}
}

// DO NOT REORDER THE FIELDS: the drop order is important!
struct StringIter<'a> {
	iter: core::str::Chars<'a>,
	_string: Rc<str>,
}

impl<'a> StringIter<'a> {
	fn new(string: Rc<str>) -> Self {
		let iter = string.chars();
		// SAFETY: the reference held by `iter` is valid as long as `string´ isn't dropped
		unsafe {
			let iter = core::mem::transmute(iter);
			Self {
				_string: string,
				iter,
			}
		}
	}
}

impl Iterator for StringIter<'_> {
	type Item = Variant;

	fn next(&mut self) -> Option<Self::Item> {
		self.iter.next().map(Variant::Char)
	}
}
