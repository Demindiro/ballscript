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
use core::intrinsics::unlikely;
use core::mem;
use tracer::*;

pub struct CallArgs {
	store_in: Option<u16>,
	func: Rc<str>,
	args: Box<[u16]>,
}

pub struct SelfCallArgs {
	store_in: Option<u16>,
	func: u16,
	args: Box<[u16]>,
}

pub enum Instruction {
	Call(u16, Box<CallArgs>),
	CallSelf(Box<SelfCallArgs>),
	CallGlobal(Box<CallArgs>),

	Jmp(u32),
	JmpIf(u16, u32),
	JmpNotIf(u16, u32),
	RetSome(u16),
	RetNone,

	Iter(u16, u16, u32),
	IterJmp(u16, u32),
	IterInt {
		reg: u16,
		from: u16,
		to: u16,
		step: u16,
		jmp_ip: u32,
	},
	IterIntJmp(u16, u32),

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
	Not(u16, u16),
	Neg(u16, u16),

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
	max_call_args: u16,
}

pub struct RunState<'a, V>
where
	V: VariantType,
{
	vars: &'a mut [V],
}

#[derive(Debug)]
pub enum RunError {
	IpOutOfBounds,
	RegisterOutOfBounds,
	NoIterator,
	UndefinedFunction,
	CallError(Box<CallError>),
	IncorrectArgumentCount,
	ArgumentOutOfBounds,
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
	($state:ident, $r:ident = $a:ident $op:tt) => {
		reg!(mut $state $r) = reg!(ref $state $a).$op().map_err(err::call)?;
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
		functions: &[Self],
		locals: &mut [V],
		args: &[&V],
		env: &Environment<V>,
		tracer: &T,
	) -> Result<V, RunError>
	where
		T: Tracer<V>,
	{
		if args.len() != self.param_count as usize {
			return Err(RunError::IncorrectArgumentCount);
		}

		/*
		let vars_len = self.var_count as usize + self.consts.len();
		let mut vars = Vec::with_capacity(vars_len);
		vars.extend(args.iter().copied().cloned());
		vars.resize_with(self.var_count as usize, || V::default());
		vars.extend(self.consts.iter().cloned());
		let vars = &mut vars[..];
		*/

		// This version is faster but obviously not as safe
		// The "zero-cost" abstractions turned out to be slower, so there is some
		// manual work involved now.
		// Hopefully the safe version will be as fast as this eventually.
		let vars_len = self.var_count as usize + self.consts.len();
		let mut vars = Vec::with_capacity(vars_len);
		vars.resize_with(vars_len, || mem::MaybeUninit::uninit());
		// Initializes all elements from 0 to args.len()
		for (i, v) in args
			.iter()
			.copied()
			.cloned()
			.map(|v| mem::MaybeUninit::new(v))
			.enumerate()
		{
			vars[i] = v;
		}
		// Initializes all elements from args.len() to self.var_count
		for i in args.len()..self.var_count as usize {
			vars[i] = mem::MaybeUninit::new(V::default());
		}
		// Initializes all elements from self.var_count to (self.var_count + self.consts.len())
		// vars_len == self.var_count + self.consts.len(), hence this initializes the remaining
		// elements.
		for (i, v) in self
			.consts
			.iter()
			.cloned()
			.map(|v| mem::MaybeUninit::new(v))
			.enumerate()
		{
			vars[i + self.var_count as usize] = v;
		}
		let vars = &mut vars[..];
		// SAFETY: all variables are initialized
		let vars = unsafe { mem::transmute(vars) };

		let mut call_args = Vec::new();
		call_args.resize(self.max_call_args as usize, core::ptr::null());
		let call_args = &mut call_args[..];

		let mut state = RunState { vars };

		struct IterIntState {
			current: isize,
			step: isize,
			stop: isize,
		}

		let mut ip = 0;
		let mut iterators = Vec::new();
		let mut iterators_int = Vec::new();

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
						// Set arguments
						if unlikely(call_args.len() < args.len()) {
							return Err(RunError::ArgumentOutOfBounds);
						}
						for (i, a) in args.iter().enumerate() {
							call_args[i] = reg!(ref state a) as *const V;
						}
						// SAFETY: All the pointers are valid references.
						let ca: &[&V] = unsafe { mem::transmute(&call_args[..args.len()]) };

						// Perform call
						let obj = reg!(ref state reg);
						let trace_call = TraceCall::new(tracer, self, func);
						let r = obj.call(func, ca, env).map_err(err::call)?;
						mem::drop(trace_call);

						// Store return value
						if let Some(reg) = store_in {
							reg!(mut state reg) = r;
						}
					}
					CallGlobal(box CallArgs {
						store_in,
						func,
						args,
					}) => {
						// Set arguments
						if unlikely(call_args.len() < args.len()) {
							return Err(RunError::ArgumentOutOfBounds);
						}
						for (i, a) in args.iter().enumerate() {
							call_args[i] = reg!(ref state a) as *const V;
						}
						// SAFETY: All the pointers are valid references.
						let ca: &[&V] = unsafe { mem::transmute(&call_args[..args.len()]) };

						// Perform call
						let trace_call = TraceCall::new(tracer, self, func);
						let r = env.call(func, ca).map_err(err::call)?;
						mem::drop(trace_call);

						// Store return value
						if let Some(reg) = store_in {
							reg!(mut state reg) = r;
						}
					}
					CallSelf(box SelfCallArgs {
						store_in,
						func,
						args,
					}) => {
						// Set arguments
						if unlikely(call_args.len() < args.len()) {
							return Err(RunError::ArgumentOutOfBounds);
						}
						for (i, a) in args.iter().enumerate() {
							call_args[i] = reg!(ref state a) as *const V;
						}
						// SAFETY: All the pointers are valid references.
						let ca: &[&V] = unsafe { mem::transmute(&call_args[..args.len()]) };

						// Perform call
						#[cfg(not(feature = "unsafe-loop"))]
						let r = functions.get(*func as usize).ok_or(RunError::UndefinedFunction)?;
						#[cfg(feature = "unsafe-loop")]
						let r = unsafe { functions.get_unchecked(*func as usize) };
						let trace_call = TraceSelfCall::new(tracer, self, *func);
						let r = r.run(functions, locals, ca, env, tracer)?;
						mem::drop(trace_call);

						// Store return value
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
							#[cfg(not(feature = "unsafe-loop"))]
							let _ = iterators.pop().unwrap();
							#[cfg(feature = "unsafe-loop")]
							let _ = unsafe { iterators.pop().unwrap_unchecked() };
						}
					}
					IterInt {
						reg,
						from,
						to,
						step,
						jmp_ip,
					} => {
						let from = reg!(ref state from)
							.as_integer()
							.map_err(|_| RunError::IncompatibleType)?;
						let to = reg!(ref state to)
							.as_integer()
							.map_err(|_| RunError::IncompatibleType)?;
						let step = reg!(ref state step)
							.as_integer()
							.map_err(|_| RunError::IncompatibleType)?;
						if from != to {
							reg!(mut state reg) = V::new_integer(from);
							iterators_int.push(IterIntState {
								current: from,
								stop: to,
								step,
							});
						} else {
							ip = *jmp_ip
						}
					}
					IterIntJmp(reg, jmp_ip) => {
						#[cfg(not(feature = "unsafe-loop"))]
						let iter = iterators_int.last_mut().ok_or(RunError::NoIterator)?;
						#[cfg(feature = "unsafe-loop")]
						let iter = unsafe {
							let i = iterators_int.len() - 1;
							iterators_int.get_unchecked_mut(i)
						};
						iter.current += iter.step;
						// >= is preferred over > so that step == 0 loops forever
						if (iter.step >= 0 && iter.current < iter.stop)
							|| (iter.step < 0 && iter.current > iter.stop)
						{
							reg!(mut state reg) = V::new_integer(iter.current);
							ip = *jmp_ip;
						} else {
							#[cfg(not(feature = "unsafe-loop"))]
							let _ = iterators_int.pop().unwrap();
							#[cfg(feature = "unsafe-loop")]
							let _ = unsafe { iterators_int.pop().unwrap_unchecked() };
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
					Neg(r, a) => run_op!(state, r = a neg),
					Not(r, a) => run_op!(state, r = a not),
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

impl<V> RunState<'_, V>
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
			IterInt {
				reg,
				from,
				to,
				step,
				jmp_ip,
			} => write!(f, "iterint {}, {}, {}, {}, {}", reg, from, to, step, jmp_ip),
			IterIntJmp(r, p) => write!(f, "iterijp {}, {}", r, p),

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
			Neg(r, a) => write!(f, "neg     {}, {}", r, a),
			Not(r, a) => write!(f, "not     {}, {}", r, a),

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

impl fmt::Debug for CallArgs {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		if let Some(n) = self.store_in {
			write!(f, "{}, \"{}\", {:?}", n, self.func, self.args)
		} else {
			write!(f, "none, \"{}\", {:?}", self.func, self.args)
		}
	}
}

impl fmt::Debug for SelfCallArgs {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
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
