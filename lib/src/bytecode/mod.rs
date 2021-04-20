// Copyright (C) 2021  David Hoppenbrouwers
//
// This file is licensed under the MIT license. See LICENSE for details.

mod builder;
mod tracer;

pub(crate) use builder::{ByteCodeBuilder, ByteCodeError};
pub use tracer::Tracer;

use crate::script::CallError;
use crate::std_types::*;
use crate::{Array, Dictionary, Environment, VariantType};
use core::fmt::{self, Debug, Formatter};
use core::mem;
use tracer::*;

pub struct CallArgs {
	store_in: Option<u16>,
	func: Rc<str>,
	args: Box<[u16]>,
}

pub enum Instruction {
	Call(u16, Box<CallArgs>),
	CallSelf(Box<CallArgs>),
	CallGlobal(Box<CallArgs>),

	Jmp(u32),
	JmpIf(u16, u32),
	JmpNotIf(u16, u32),
	RetSome(u16),
	RetNone,

	Iter(u16, u16, u32),
	IterJmp(u16, u32),

	Add(u16, u16, u16),
	Sub(u16, u16, u16),
	Mul(u16, u16, u16),
	Div(u16, u16, u16),
	Rem(u16, u16, u16),
	And(u16, u16, u16),
	Or(u16, u16, u16),
	Xor(u16, u16, u16),
	Shl(u16, u16, u16),
	Shr(u16, u16, u16),

	LessEq(u16, u16, u16),
	Less(u16, u16, u16),
	Neq(u16, u16, u16),
	Eq(u16, u16, u16),

	Store(u16, u16),
	Load(u16, u16),
	Move(u16, u16),

	NewArray(u16, usize),
	NewDictionary(u16, usize),
	GetIndex(u16, u16, u16),
	SetIndex(u16, u16, u16),
}

pub struct ByteCode<V>
where
	V: VariantType,
{
	code: Vec<Instruction>,
	param_count: u16,
	var_count: u16,
	consts: Vec<V>,
	name: Rc<str>,
}

pub struct RunState<V>
where
	V: VariantType,
{
	vars: Vec<V>,
}

#[derive(Debug)]
pub enum RunError {
	IpOutOfBounds,
	RegisterOutOfBounds,
	NoIterator,
	UndefinedFunction,
	CallError(Box<CallError>),
	IncorrectArgumentCount,
	IncompatibleType,
	NotBoolean,
	LocalOutOfBounds,
}

pub type CallResult<T> = Result<T, CallError>;

#[cfg(not(feature = "unsafe-loop"))]
macro_rules! reg {
	(ref $state:ident $reg:ident) => {
		$state.vars.get(*$reg as usize).ok_or(RunError::RegisterOutOfBounds)?
	};
	(mut $state:ident $reg:ident) => {
		*reg!(ref mut $state $reg)
	};
	(ref mut $state:ident $reg:expr) => {
		$state.vars.get_mut(*$reg as usize).ok_or(RunError::RegisterOutOfBounds)?
	};
}

#[cfg(feature = "unsafe-loop")]
macro_rules! reg {
	(ref $state:ident $reg:ident) => {
		unsafe { $state.vars.get_unchecked(*$reg as usize) }
	};
	(mut $state:ident $reg:ident) => {
		*reg!(ref mut $state $reg)
	};
	(ref mut $state:ident $reg:expr) => {
		unsafe { $state.vars.get_unchecked_mut(*$reg as usize) }
	};
}

macro_rules! run_op {
	($state:ident, $r:ident = $a:ident $op:tt $b:ident) => {
		reg!(mut $state $r) = (reg!(ref $state $a).$op(reg!(ref $state $b))).map_err(err::call)?;
	};
}

macro_rules! run_cmp {
	($state:ident, $r:ident = $a:ident $op:tt $b:ident) => {
		reg!(mut $state $r) = (reg!(ref $state $a) $op reg!(ref $state $b)).into();
	};
}

impl<V> ByteCode<V>
where
	V: VariantType,
{
	pub(crate) fn run<T>(
		&self,
		functions: &FxHashMap<Rc<str>, Self>,
		locals: &mut [V],
		args: &[V],
		env: &Environment<V>,
		tracer: &T,
	) -> Result<V, RunError>
	where
		T: Tracer<V>,
	{
		if args.len() != self.param_count as usize {
			return Err(RunError::IncorrectArgumentCount);
		}

		let mut vars = Vec::with_capacity(self.var_count as usize + self.consts.len());
		for a in args.iter() {
			vars.push(a.clone());
		}
		vars.resize(self.var_count as usize, V::default());
		vars.extend(self.consts.iter().cloned());

		let mut state = RunState { vars };

		let mut ip = 0;
		let mut iterators = Vec::new();
		let mut call_args = Vec::new();

		let _trace_run = TraceRun::new(tracer, self);

		let ret = loop {
			if let Some(instr) = self.code.get(ip as usize) {
				let _trace_instruction = TraceInstruction::new(tracer, self, ip, instr);
				tracer.peek(self, &mut state);
				ip += 1;
				use Instruction::*;
				match instr {
					Call(
						reg,
						box CallArgs {
							store_in,
							func,
							args,
						},
					) => {
						for a in args.iter() {
							call_args.push(reg!(ref state a).clone());
						}
						let obj = reg!(ref state reg);
						let trace_call = TraceCall::new(tracer, self, func);
						let r = obj.call(func, &call_args[..], env).map_err(err::call)?;
						mem::drop(trace_call);
						call_args.clear();
						if let Some(reg) = store_in {
							reg!(mut state reg) = r;
						}
					}
					CallGlobal(box CallArgs {
						store_in,
						func,
						args,
					}) => {
						for a in args.iter() {
							call_args.push(reg!(ref state a).clone());
						}
						let trace_call = TraceCall::new(tracer, self, func);
						let r = env.call(func, &call_args[..]).map_err(err::call)?;
						mem::drop(trace_call);
						call_args.clear();
						if let Some(reg) = store_in {
							reg!(mut state reg) = r;
						}
					}
					CallSelf(box CallArgs {
						store_in,
						func,
						args,
					}) => {
						for a in args.iter() {
							call_args.push(reg!(ref state a).clone());
						}
						let r = functions.get(func).ok_or(RunError::UndefinedFunction)?;
						let trace_call = TraceCall::new(tracer, self, func);
						let r = r.run(functions, locals, &call_args[..], env, tracer)?;
						mem::drop(trace_call);
						call_args.clear();
						if let Some(reg) = store_in {
							reg!(mut state reg) = r;
						}
					}
					RetSome(reg) => break Ok(mem::take(reg!(ref mut state reg))),
					RetNone => break Ok(V::default()),
					Iter(reg, iter, jmp_ip) => {
						let iter = reg!(ref state iter);
						let mut iter = iter.iter().map_err(err::call)?;
						if let Some(e) = iter.next() {
							reg!(mut state reg) = e;
							iterators.push(iter);
						} else {
							ip = *jmp_ip;
						}
					}
					IterJmp(reg, jmp_ip) => {
						#[cfg(not(feature = "unsafe-loop"))]
						let iter = iterators.last_mut().ok_or(RunError::NoIterator)?;
						#[cfg(feature = "unsafe-loop")]
						let iter = unsafe {
							let i = iterators.len() - 1;
							iterators.get_unchecked_mut(i)
						};
						if let Some(e) = iter.next() {
							reg!(mut state reg) = e;
							ip = *jmp_ip;
						} else {
							let _ = iterators.pop().unwrap();
						}
					}
					JmpIf(reg, jmp_ip) => {
						if let Ok(b) = mem::take(reg!(ref mut state reg)).as_bool() {
							reg!(mut state reg) = V::new_bool(b);
							if !b {
								ip = *jmp_ip;
							}
						} else {
							return Err(RunError::NotBoolean);
						}
					}
					JmpNotIf(reg, jmp_ip) => {
						if let Ok(b) = mem::take(reg!(ref mut state reg)).as_bool() {
							reg!(mut state reg) = V::new_bool(b);
							if b {
								ip = *jmp_ip;
							}
						} else {
							return Err(RunError::NotBoolean);
						}
					}
					Jmp(jmp_ip) => ip = *jmp_ip,
					Add(r, a, b) => run_op!(state, r = a add b),
					Sub(r, a, b) => run_op!(state, r = a sub b),
					Mul(r, a, b) => run_op!(state, r = a mul b),
					Div(r, a, b) => run_op!(state, r = a div b),
					Rem(r, a, b) => run_op!(state, r = a rem b),
					And(r, a, b) => run_op!(state, r = a bitand b),
					Or(r, a, b) => run_op!(state, r = a bitor b),
					Xor(r, a, b) => run_op!(state, r = a bitxor b),
					Shl(r, a, b) => run_op!(state, r = a lhs b),
					Shr(r, a, b) => run_op!(state, r = a rhs b),
					LessEq(r, a, b) => run_cmp!(state, r = a <= b),
					Less(r, a, b) => run_cmp!(state, r = a < b),
					Neq(r, a, b) => run_cmp!(state, r = a != b),
					Eq(r, a, b) => run_cmp!(state, r = a == b),
					Store(r, l) => {
						*locals
							.get_mut(*l as usize)
							.ok_or(RunError::LocalOutOfBounds)? = reg!(ref state r).clone();
					}
					Load(r, l) => {
						reg!(mut state r) = locals
							.get(*l as usize)
							.ok_or(RunError::LocalOutOfBounds)?
							.clone();
					}
					Move(d, s) => reg!(mut state d) = reg!(ref state s).clone(),
					NewArray(r, c) => {
						reg!(mut state r) = V::new_object(Rc::new(Array::with_len(*c)))
					}
					NewDictionary(r, c) => {
						let d = Rc::new(Dictionary::with_capacity(*c));
						reg!(mut state r) = V::new_object(d);
					}
					GetIndex(r, o, i) => {
						reg!(mut state r) = reg!(ref state o)
							.index(reg!(ref state i))
							.map_err(err::call)?
					}
					SetIndex(r, o, i) => reg!(ref state o)
						.set_index(reg!(ref state i), reg!(ref state r).clone())
						.map_err(err::call)?,
				}
			} else {
				break Err(RunError::IpOutOfBounds);
			}
		};

		ret
	}

	pub fn name(&self) -> &Rc<str> {
		&self.name
	}
}

impl<V> RunState<V>
where
	V: VariantType,
{
	pub fn variables(&mut self) -> &mut [V] {
		&mut self.vars[..]
	}
}

/// This returns each instruction on oneline instead of 5+ with the default Debug
impl Debug for Instruction {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		use Instruction::*;
		match self {
			Call(r, a) => write!(f, "call    {}, {:?}", r, a),
			CallSelf(a) => write!(f, "call    self, {:?}", a),
			CallGlobal(a) => write!(f, "call    env, {:?}", a),
			RetSome(reg) => write!(f, "ret     {}", reg),
			RetNone => write!(f, "ret     none"),

			Iter(r, i, p) => write!(f, "iter    {}, {}, {}", r, i, p),
			IterJmp(r, p) => write!(f, "iterjmp {}, {}", r, p),
			JmpIf(r, p) => write!(f, "jmpif   {}, {}", r, p),
			JmpNotIf(r, p) => write!(f, "jmpnif  {}, {}", r, p),
			Jmp(p) => write!(f, "jmp     {}", p),

			Add(r, a, b) => write!(f, "add     {}, {}, {}", r, a, b),
			Sub(r, a, b) => write!(f, "sub     {}, {}, {}", r, a, b),
			Mul(r, a, b) => write!(f, "mul     {}, {}, {}", r, a, b),
			Div(r, a, b) => write!(f, "div     {}, {}, {}", r, a, b),
			Rem(r, a, b) => write!(f, "rem     {}, {}, {}", r, a, b),
			And(r, a, b) => write!(f, "and     {}, {}, {}", r, a, b),
			Or(r, a, b) => write!(f, "or      {}, {}, {}", r, a, b),
			Xor(r, a, b) => write!(f, "xor     {}, {}, {}", r, a, b),
			Shl(r, a, b) => write!(f, "shl     {}, {}, {}", r, a, b),
			Shr(r, a, b) => write!(f, "shr     {}, {}, {}", r, a, b),

			Eq(r, a, b) => write!(f, "eq      {}, {}, {}", r, a, b),
			Neq(r, a, b) => write!(f, "neq     {}, {}, {}", r, a, b),
			Less(r, a, b) => write!(f, "less    {}, {}, {}", r, a, b),
			LessEq(r, a, b) => write!(f, "lesseq  {}, {}, {}", r, a, b),

			Store(r, a) => write!(f, "store   {}, {}", r, a),
			Load(r, a) => write!(f, "load    {}, {}", r, a),
			Move(a, b) => write!(f, "move    {}, {}", a, b),

			NewArray(r, c) => write!(f, "newarr  {}, {}", r, c),
			NewDictionary(r, c) => write!(f, "newdict {}, {}", r, c),
			GetIndex(r, o, i) => write!(f, "geti    {}, {}, {}", r, o, i),
			SetIndex(r, o, i) => write!(f, "seti    {}, {}, {}", r, o, i),
		}
	}
}

impl Debug for CallArgs {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		if let Some(n) = self.store_in {
			write!(f, "{}, \"{}\", {:?}", n, self.func, self.args)
		} else {
			write!(f, "none, \"{}\", {:?}", self.func, self.args)
		}
	}
}

impl<V> fmt::Debug for ByteCode<V>
where
	V: VariantType,
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let br = |f: &mut fmt::Formatter| f.write_str(if f.alternate() { "\n" } else { ", " });
		if f.alternate() {
			f.write_str("\n")?;
		}
		write!(f, "parameters: {}", self.param_count)?;
		br(f)?;
		write!(f, "mutable variables: {}", self.var_count)?;
		br(f)?;
		f.write_str("consts:")?;
		for (i, c) in self.consts.iter().enumerate() {
			let i = i as u16 + self.var_count;
			if f.alternate() {
				write!(f, "\n    {:>3}: {:?}", i, c)?;
			} else {
				write!(f, " {}: {:?},", i, c)?;
			}
		}
		br(f)?;
		f.write_str("code:")?;
		for (i, c) in self.code.iter().enumerate() {
			if f.alternate() {
				write!(f, "\n    {:>3}: {:?}", i, c)?;
			} else {
				write!(f, "{}: {:?}", i, c)?;
			}
		}
		Ok(())
	}
}

mod err {
	use super::*;

	#[inline(never)]
	#[cold]
	pub fn call(e: CallError) -> RunError {
		RunError::CallError(Box::new(e))
	}
}