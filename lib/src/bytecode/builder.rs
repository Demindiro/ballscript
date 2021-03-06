// Copyright (C) 2021  David Hoppenbrouwers
//
// This file is licensed under the MIT license. See LICENSE for details.

use super::*;
use crate::ast::{Atom, Expression, Function, Lines, Statement, UnaryOp};
use crate::std_types::hash_map::Entry;
use crate::std_types::*;
use crate::tokenizer::{AssignOp, Op};
use crate::{Rc, VariantType};
use core::convert::TryInto;
use core::hash;
use core::ops::Deref;
use core::ptr;
use unwrap_none::UnwrapNone;

pub(crate) struct ByteCodeBuilder<'e, 's: 'e, V>
where
	V: VariantType,
{
	methods: &'e FxHashMap<Rc<str>, u8>,
	locals: &'e FxHashMap<Rc<str>, u8>,
	instr: Vec<Instruction>,
	vars: FxHashMap<&'s str, u8>,
	consts: Vec<V>,
	curr_var_count: u8,
	min_var_count: u8,
	param_count: u8,
	loops: Vec<LoopContext>,
	const_map: FxHashMap<Constant, u8>,
	string_map: &'e mut FxHashSet<Rc<str>>,
	jump_indices: Vec<(u32, u32)>,
}

enum LoopType {
	While,
	ForGeneric,
	ForInteger,
}

struct LoopContext {
	loop_type: LoopType,
	continues: Vec<u32>,
	breaks: Vec<u32>,
}

pub struct ByteCodeError<'a> {
	pub line: u32,
	pub column: u32,
	error: ByteCodeErrorType<'a>,
}

pub enum ByteCodeErrorType<'a> {
	DuplicateParameter(&'a str),
	DuplicateVariable(&'a str),
	UndefinedVariable(&'a str),
	UnexpectedBreak(),
	UnexpectedContinue(),
	TooManyRegisters(),
	Unsupported(&'a str),
	UndefinedFunction(&'a str),
	CantAssign(&'a str),
}

macro_rules! err {
	($line:expr, $column:expr, $error:ident) => {
		err!($line, $column, $error, )
	};
	($line:expr, $column:expr, $error:ident, $($arg:expr)*) => {
		return Err(ByteCodeError::new($line, $column, ByteCodeErrorType::$error($($arg,)*)));
	};
	(lazy $line:expr, $column:expr, $error:ident) => {
		err!(lazy $line, $column, $error, )
	};
	(lazy $line:expr, $column:expr, $error:ident, $($arg:expr)*) => {
		|| { ByteCodeError::new($line, $column, ByteCodeErrorType::$error($($arg,)*)) }
	};
}

/// A type used to prevent duplication of constant values. It is capable of
/// hashing and ordering `Real` types.
#[derive(Clone, Debug, PartialOrd)]
enum Constant {
	Bool(bool),
	Int(isize),
	Real(f64),
	Str(Rc<str>),
}

impl<'e, 's: 'e, V> ByteCodeBuilder<'e, 's, V>
where
	V: VariantType,
{
	pub(crate) fn parse(
		function: Function<'s>,
		methods: &'e FxHashMap<Rc<str>, u8>,
		locals: &'e FxHashMap<Rc<str>, u8>,
		string_map: &'e mut FxHashSet<Rc<str>>,
	) -> Result<ByteCode<V>, ByteCodeError<'s>> {
		let mut builder = Self {
			instr: Vec::new(),
			vars: HashMap::with_hasher(Default::default()),
			consts: Vec::new(),
			curr_var_count: function.parameters.len() as u8,
			min_var_count: function.parameters.len() as u8,
			locals,
			methods,
			param_count: function.parameters.len() as u8,
			loops: Vec::new(),
			const_map: HashMap::with_hasher(Default::default()),
			string_map,
			jump_indices: Vec::new(),
		};
		for p in function.parameters {
			if builder.vars.insert(p, builder.vars.len() as u8).is_some() {
				err!(0, 0, DuplicateParameter, p);
			}
		}
		builder.parse_block(function.lines)?;
		match builder.instr.last() {
			Some(Instruction::RetSome(_)) | Some(Instruction::RetNone) => (),
			_ => builder.instr.push(Instruction::RetNone),
		}

		if !builder.consts.is_empty() {
			// All consts are using the upper-most registers, move them downwards
			let offset = (u8::MAX - builder.consts.len() as u8).wrapping_add(1);
			let min_var_count = builder.min_var_count;
			for i in builder.instr.iter_mut() {
				use Instruction::*;
				let conv = |c: &mut u8| {
					if *c >= offset {
						*c = u8::MAX - *c + min_var_count
					}
				};
				match i {
					Call(a, box CallArgs { args, .. }) => {
						conv(a);
						for a in args.iter_mut() {
							conv(a);
						}
					}
					CallEnv {
						args: box CallArgs { args, .. },
					} => {
						for a in args.iter_mut() {
							conv(a);
						}
					}
					CallSelf { args, .. } => {
						args.iter_mut().for_each(conv);
					}
					JmpIf(a, _)
					| JmpNotIf(a, _)
					| Iter(_, a, _)
					| RetSome(a)
					| Neg(_, a)
					| Not(_, a)
					| Store(a, _)
					| Load(a, _)
					| Move(_, a) => conv(a),
					Add(_, a, b)
					| Sub(_, a, b)
					| Mul(_, a, b)
					| Div(_, a, b)
					| Rem(_, a, b)
					| And(_, a, b)
					| Or(_, a, b)
					| Xor(_, a, b)
					| Shr(_, a, b)
					| Shl(_, a, b)
					| Eq(_, a, b)
					| Neq(_, a, b)
					| Less(_, a, b)
					| LessEq(_, a, b)
					| SetIndex(a, _, b)
					| GetIndex(a, _, b) => {
						conv(a);
						conv(b);
					}
					IterInt {
						from: a,
						to: b,
						step: c,
						..
					} => {
						conv(a);
						conv(b);
						conv(c);
					}
					IterJmp(_, _)
					| IterIntJmp(_, _)
					| Break { .. }
					| Jmp(_)
					| RetNone
					| CopySelf { .. }
					| NewArray(_, _)
					| NewDictionary(_, _) => (),
				}
			}
		}

		let name = builder.map_string(function.name);

		let mut code = builder.instr.into_boxed_slice();

		for (instr, jmp) in builder.jump_indices {
			assert!((jmp as usize) < code.len(), "Jump index out of bounds");
			let code_ptr = code.as_ptr();
			use Instruction::*;
			match &mut code[instr as usize] {
				Jmp(jp)
				| JmpIf(_, jp)
				| JmpNotIf(_, jp)
				| Iter(_, _, jp)
				| IterJmp(_, jp)
				| IterInt { jmp_ip: jp, .. }
				| IterIntJmp(_, jp)
				| Break { jmp_ip: jp, .. } => *jp = unsafe { code_ptr.offset(jmp as isize) },
				b => panic!("Not a branching instruction: {}:{}  {:?}", instr, jmp, b),
			}
		}

		Ok(ByteCode {
			code,
			var_count: builder.min_var_count,
			param_count: builder.param_count,
			consts: builder.consts,
			name,
		})
	}

	fn parse_block(&mut self, lines: Lines<'s>) -> Result<(), ByteCodeError<'s>> {
		let mut frame_vars = Vec::new();
		for line in lines {
			match line {
				Statement::Expression { expr, .. } => {
					self.parse_expression(None, expr)?;
				}
				Statement::For {
					var,
					from,
					to,
					step,
					lines,
					line,
					column,
				} => {
					let og_cvc = self.curr_var_count;
					let (l, c) = (line, column);

					// Parse `to` expression
					let iter_reg = self.alloc_reg(l, c)?;
					let iter_reg = if let Some(r) = self.parse_expression(Some(iter_reg), to)? {
						self.dealloc_reg();
						r
					} else {
						iter_reg
					};

					// Parse `from` and `step` expressions, if any
					let from_step = if let Some(from) = from {
						let from_reg = self.alloc_reg(l, c)?;
						let from = if let Some(r) = self.parse_expression(Some(from_reg), from)? {
							self.dealloc_reg();
							r
						} else {
							from_reg
						};
						let step = if let Some(step) = step {
							let step_reg = self.alloc_reg(l, c)?;
							if let Some(r) = self.parse_expression(Some(step_reg), step)? {
								self.dealloc_reg();
								r
							} else {
								step_reg
							}
						} else {
							self.add_const(V::new_integer(1))
						};
						Some((from, step))
					} else if let Some(step) = step {
						let from = self.add_const(V::new_integer(0));
						let step_reg = self.alloc_reg(l, c)?;
						let step = if let Some(r) = self.parse_expression(Some(step_reg), step)? {
							self.dealloc_reg();
							r
						} else {
							step_reg
						};
						Some((from, step))
					} else if self
						.get_const(iter_reg)
						.and_then(|v| v.as_integer().ok())
						.is_some()
					{
						let from = self.add_const(V::new_integer(0));
						let step = self.add_const(V::new_integer(1));
						Some((from, step))
					} else {
						None
					};
					self.update_min_vars();

					// Insert var and iter instruction
					let var_reg = self.alloc_reg(l, c)?;
					self.vars.insert(var, var_reg).expect_none(var);
					if let Some((from, step)) = from_step {
						self.instr.push(Instruction::IterInt {
							reg: var_reg.try_into().expect("TODO"),
							from,
							to: iter_reg,
							step,
							jmp_ip: ptr::null(),
						});
					} else {
						self.instr
							.push(Instruction::Iter(var_reg, iter_reg, ptr::null()));
					};
					let ic = self.instr.len() - 1;
					let ip = self.instr.len() as u32;

					// Parse loop block
					self.loops.push(LoopContext {
						loop_type: if from_step.is_some() {
							LoopType::ForInteger
						} else {
							LoopType::ForGeneric
						},
						continues: Vec::new(),
						breaks: Vec::new(),
					});
					self.parse_block(lines)?;
					let context = self.loops.pop().unwrap();

					// Make `continue`s jump to the `IterJmp` instruction
					for i in context.continues {
						let ip = self.instr.len() as u32;
						self.jump_indices.push((i, ip));
					}

					// Insert `IterJmp` instruction & update the `Iter` with the end address.
					{
						let i = self.instr.len() as u32;
						self.jump_indices.push((i, ip));
					}
					if from_step.is_none() {
						self.instr.push(Instruction::IterJmp(var_reg, ptr::null()));
					} else {
						self.instr
							.push(Instruction::IterIntJmp(var_reg, ptr::null()));
					}
					let ip = self.instr.len() as u32;
					self.jump_indices.push((ic as u32, ip));

					// Make `break`s jump to right after the `IterJmp` instruction
					for i in context.breaks {
						let ip = self.instr.len() as u32;
						self.jump_indices.push((i, ip));
					}

					// Remove loop variable
					self.vars.remove(var).expect(var);

					self.curr_var_count = og_cvc;
				}
				Statement::While { expr, lines, .. } => {
					let og_cvc = self.curr_var_count;

					// Insert `Jmp` to the expr evaluation
					let start_ip = self.instr.len();
					self.instr.push(Instruction::Jmp(ptr::null()));

					// Parse loop block
					self.loops.push(LoopContext {
						loop_type: LoopType::While,
						continues: Vec::new(),
						breaks: Vec::new(),
					});
					self.parse_block(lines)?;
					let context = self.loops.pop().unwrap();

					// Make `continue`s jump to the expression evaluation
					for i in context.continues {
						let ip = self.instr.len() as u32;
						self.jump_indices.push((i, ip));
					}

					// Update start jump
					let ip = self.instr.len() as u32;
					self.jump_indices.push((start_ip as u32, ip));

					// Parse expression
					let expr_reg = self.curr_var_count;
					self.curr_var_count += 1;
					let expr_reg = if let Some(r) = self.parse_expression(Some(expr_reg), expr)? {
						self.curr_var_count -= 1;
						r
					} else {
						expr_reg
					};
					self.jump_indices
						.push((self.instr.len() as u32, start_ip as u32 + 1));
					self.instr
						.push(Instruction::JmpNotIf(expr_reg, ptr::null()));

					// Make `break`s jump to right after the expression evaluation
					for i in context.breaks {
						let ip = self.instr.len() as u32;
						self.jump_indices.push((i, ip));
					}

					self.curr_var_count = og_cvc;
				}
				Statement::If {
					expr,
					lines,
					else_lines,
					..
				} => {
					// If
					let expr = self.parse_expression(Some(self.curr_var_count), expr)?;
					let expr = if let Some(expr) = expr {
						expr
					} else {
						self.curr_var_count += 1;
						self.curr_var_count - 1
					};
					self.instr.push(Instruction::JmpIf(expr, ptr::null()));
					let ic = self.instr.len() as u32 - 1;
					self.parse_block(lines)?;
					// Skip else
					let skip_else_jmp = else_lines.as_ref().map(|_| {
						self.instr.push(Instruction::Jmp(ptr::null()));
						self.instr.len() as u32 - 1
					});
					// Jump to right after `if` block if `expr` evaluates to false
					let ip = self.instr.len() as u32;
					self.jump_indices.push((ic, ip));
					// Else
					if let Some(else_lines) = else_lines {
						self.parse_block(else_lines)?;
						let ip = self.instr.len() as u32;
						self.jump_indices.push((skip_else_jmp.unwrap(), ip));
					}
				}
				Statement::Return { expr, .. } => {
					if let Some(expr) = expr {
						let r = self.parse_expression(Some(0), expr)?.unwrap_or(0);
						self.instr.push(Instruction::RetSome(r));
					} else {
						self.instr.push(Instruction::RetNone);
					}
				}
				Statement::Assign {
					var,
					assign_op,
					expr,
					line,
					column,
				} => {
					// Get the register or property to which much be assigned
					match var {
						Expression::Atom { atom, .. } => match atom {
							Atom::Name(var) => {
								if let Some(&reg) = self.vars.get(var) {
									let expr = self.parse_expression(Some(reg), expr)?;
									if let AssignOp::None = assign_op {
										if let Some(expr) = expr {
											self.instr.push(Instruction::Move(reg, expr));
										}
									} else {
										let expr = if let Some(expr) = expr { expr } else { reg };
										self.instr.push(match assign_op {
											AssignOp::None => unreachable!(),
											AssignOp::Add => Instruction::Add(reg, reg, expr),
											AssignOp::Sub => Instruction::Sub(reg, reg, expr),
											AssignOp::Mul => Instruction::Mul(reg, reg, expr),
											AssignOp::Div => Instruction::Div(reg, reg, expr),
											AssignOp::Rem => Instruction::Rem(reg, reg, expr),
											AssignOp::And => Instruction::And(reg, reg, expr),
											AssignOp::Or => Instruction::Or(reg, reg, expr),
											AssignOp::Xor => Instruction::Xor(reg, reg, expr),
										});
									}
								} else {
									err!(line, column, UndefinedVariable, var);
								}
							}
							Atom::_Self => err!(line, column, CantAssign, "self"),
							Atom::Env => err!(line, column, CantAssign, "env"),
							_ => err!(
								line,
								column,
								Unsupported,
								"Complex lvalues are not supported yet"
							),
						},
						Expression::Operation {
							op: Op::Access,
							left,
							right,
							line,
							column,
						} => match *left {
							Expression::Atom { atom, line, column } => match atom {
								Atom::Name(n) => todo!("{}", n),
								Atom::_Self => match *right {
									Expression::Atom {
										atom: Atom::Name(var),
										line,
										column,
									} => {
										if let Some(local) = self.locals.get(var as &str) {
											let og_cvc = self.curr_var_count;
											let e =
												self.parse_expression_new_reg(expr, line, column)?;
											if let AssignOp::None = assign_op {
											} else {
												let tmp_reg = self.curr_var_count;
												self.curr_var_count += 1;
												self.update_min_vars();
												self.instr.push(Instruction::Load(tmp_reg, *local));
												self.instr.push(match assign_op {
													AssignOp::None => unreachable!(),
													AssignOp::Add => {
														Instruction::Add(e, tmp_reg, e)
													}
													AssignOp::Sub => {
														Instruction::Sub(e, tmp_reg, e)
													}
													AssignOp::Mul => {
														Instruction::Mul(e, tmp_reg, e)
													}
													AssignOp::Div => {
														Instruction::Div(e, tmp_reg, e)
													}
													AssignOp::Rem => {
														Instruction::Rem(e, tmp_reg, e)
													}
													AssignOp::And => {
														Instruction::And(e, tmp_reg, e)
													}
													AssignOp::Or => Instruction::Or(e, tmp_reg, e),
													AssignOp::Xor => {
														Instruction::Xor(e, tmp_reg, e)
													}
												});
												self.curr_var_count -= 1;
											}
											self.instr.push(Instruction::Store(e, *local));
											self.update_min_vars();
											self.curr_var_count = og_cvc;
										} else {
											err!(line, column, UndefinedVariable, var);
										}
									}
									_ => err!(
										line,
										column,
										Unsupported,
										"Complex lvalues are not supported yet"
									),
								},
								Atom::Env => err!(
									line,
									column,
									Unsupported,
									"Environment variables are not supported yet"
								),
								_ => err!(
									line,
									column,
									Unsupported,
									"Complex lvalues are not supported yet"
								),
							},
							_ => err!(
								line,
								column,
								Unsupported,
								"Complex lvalues are not supported yet"
							),
						},
						Expression::Operation {
							op: Op::Index,
							left,
							right,
							line,
							column,
						} => {
							let og_cvc = self.curr_var_count;
							let left = self.parse_expression_new_reg(*left, line, column)?;
							let right = self.parse_expression_new_reg(*right, line, column)?;
							let expr = self.parse_expression_new_reg(expr, line, column)?;
							self.update_min_vars();
							self.instr.push(Instruction::SetIndex(expr, left, right));
							self.curr_var_count = og_cvc;
						}
						_ => err!(
							line,
							column,
							Unsupported,
							"Complex lvalues are not supported yet"
						),
					}
				}
				Statement::LooseExpression { expr, .. } => {
					self.parse_expression(None, expr)?;
				}
				Statement::Declare { var, line, column } => {
					if self.vars.insert(var, self.curr_var_count).is_none() {
						self.curr_var_count += 1;
						self.min_var_count = self.min_var_count.max(self.curr_var_count);
						frame_vars.push(var);
					} else {
						err!(line, column, DuplicateVariable, var);
					}
				}
				Statement::Continue {
					levels,
					line,
					column,
				} => {
					let i = self.loops.len().wrapping_sub(levels as usize + 1);
					let c = self
						.loops
						.get_mut(i)
						.ok_or_else(err!(lazy line, column, UnexpectedContinue))?;
					c.continues.push(self.instr.len() as u32);
					self.instr.push(Instruction::Jmp(ptr::null()));
				}
				Statement::Break {
					levels,
					line,
					column,
				} => {
					// The AST interprets level as a reverse "loop index", while we interpret it
					// as "how many loops to pop"
					let levels = levels as usize + 1;
					let i = self.loops.len().wrapping_sub(levels);
					let c = self
						.loops
						.get_mut(i)
						.ok_or_else(err!(lazy line, column, UnexpectedBreak))?;
					c.breaks.push(self.instr.len() as u32);
					let (mut amount, mut amount_int) = (0, 0);
					for l in self.loops.iter().rev().take(levels) {
						match l.loop_type {
							LoopType::While => (),
							LoopType::ForGeneric => amount += 1,
							LoopType::ForInteger => amount_int += 1,
						}
					}
					self.instr.push(if amount == 0 && amount_int == 0 {
						Instruction::Jmp(ptr::null())
					} else {
						Instruction::Break {
							amount,
							amount_int,
							jmp_ip: ptr::null(),
						}
					})
				}
			}
		}
		self.min_var_count = self.min_var_count.max(self.vars.len() as u8);
		for fv in frame_vars {
			self.vars.remove(fv).unwrap();
		}
		Ok(())
	}

	fn parse_expression_new_reg(
		&mut self,
		expr: Expression<'s>,
		line: u32,
		column: u32,
	) -> Result<u8, ByteCodeError<'s>> {
		let r = self.alloc_reg(line, column)?;
		Ok(if let Some(r) = self.parse_expression(Some(r), expr)? {
			self.dealloc_reg();
			r
		} else {
			r
		})
	}

	fn parse_expression(
		&mut self,
		store: Option<u8>,
		expr: Expression<'s>,
	) -> Result<Option<u8>, ByteCodeError<'s>> {
		match expr {
			Expression::Operation {
				left, op, right, ..
			} => {
				let store = store.expect("TODO: handle operations without store location");
				let og_cvc = self.curr_var_count;
				let (r_left, r_right) = (self.curr_var_count, self.curr_var_count + 1);
				self.curr_var_count += 2;
				let or_left = self.parse_expression(Some(r_left), *left)?;
				let left = if let Some(l) = or_left {
					self.curr_var_count -= 1;
					l
				} else {
					r_left
				};
				let or_right = self.parse_expression(Some(r_right), *right)?;
				let right = if let Some(r) = or_right {
					self.curr_var_count -= 1;
					r
				} else {
					r_right
				};
				self.update_min_vars();
				self.instr.push(match op {
					Op::Add => Instruction::Add(store, left, right),
					Op::Sub => Instruction::Sub(store, left, right),
					Op::Mul => Instruction::Mul(store, left, right),
					Op::Div => Instruction::Div(store, left, right),
					Op::Rem => Instruction::Rem(store, left, right),
					Op::And => Instruction::And(store, left, right),
					Op::Or => Instruction::Or(store, left, right),
					Op::Xor => Instruction::Xor(store, left, right),
					Op::ShiftLeft => Instruction::Shl(store, left, right),
					Op::ShiftRight => Instruction::Shr(store, left, right),
					Op::Eq => Instruction::Eq(store, left, right),
					Op::Neq => Instruction::Neq(store, left, right),
					Op::Less => Instruction::Less(store, left, right),
					Op::Greater => Instruction::Less(store, right, left),
					Op::LessEq => Instruction::LessEq(store, left, right),
					Op::GreaterEq => Instruction::LessEq(store, right, left),
					Op::Not | Op::AndThen | Op::OrElse => todo!(),
					Op::Index => Instruction::GetIndex(store, left, right),
					Op::Access => panic!("{:?} is not an actual op (bug in AST)", Op::Access),
				});
				self.curr_var_count = og_cvc;
				Ok(None)
			}
			Expression::UnaryOperation { expr, op, .. } => {
				let store = store.expect("TODO: handle operations without store location");
				let og_cvc = self.curr_var_count;
				let r_expr = self.curr_var_count;
				self.curr_var_count += 1;
				let expr = if let Some(r) = self.parse_expression(Some(r_expr), *expr)? {
					self.curr_var_count -= 1;
					r
				} else {
					r_expr
				};
				self.update_min_vars();
				self.instr.push(match op {
					UnaryOp::Neg => Instruction::Neg(store, expr),
					UnaryOp::Not => Instruction::Not(store, expr),
				});
				self.curr_var_count = og_cvc;
				Ok(None)
			}
			Expression::Atom { atom, line, column } => match atom {
				Atom::_Self => {
					let dest = store.expect("No register to store self in");
					self.instr.push(Instruction::CopySelf { dest });
					Ok(None)
				}
				Atom::Env => todo!(),
				Atom::Name(name) => {
					if let Some(&reg) = self.vars.get(name) {
						Ok(Some(reg))
					} else if let Some(&local) = self.locals.get(name) {
						let store = store.expect("No register to store local in");
						self.instr.push(Instruction::Load(store, local));
						Ok(None)
					} else {
						err!(line, column, UndefinedVariable, name)
					}
				}
				Atom::Real(r) => Ok(Some(self.add_const(V::new_real(r)))),
				Atom::Integer(i) => Ok(Some(self.add_const(V::new_integer(i)))),
				Atom::String(s) => {
					let s = V::new_string(self.map_string(s));
					Ok(Some(self.add_const(s)))
				}
				Atom::Bool(b) => Ok(Some(self.add_const(V::new_bool(b)))),
			},
			Expression::Function {
				expr,
				name,
				arguments,
				line,
				column,
			} => {
				let og_cvc = self.curr_var_count;

				enum Obj {
					_Self,
					Env,
					Some(u8),
				}

				// Parse expression on which to call the function on
				// `_Self` and `Env` are special cases however
				let expr = match expr {
					Some(box Expression::Atom {
						atom: Atom::_Self, ..
					}) => Obj::_Self,
					Some(box Expression::Atom {
						atom: Atom::Env, ..
					}) => Obj::Env,
					Some(expr) => {
						let r = self.alloc_reg(line, column)?;
						Obj::Some(if let Some(r) = self.parse_expression(Some(r), *expr)? {
							self.dealloc_reg();
							r
						} else {
							r
						})
					}
					None => {
						err!(
							line,
							column,
							Unsupported,
							"Local functions are not supported yet"
						);
					}
				};

				// Parse arguments
				let mut args = Vec::with_capacity(arguments.len());
				for a in arguments {
					let r = self.alloc_reg(line, column)?;
					if let Some(r) = self.parse_expression(Some(r), a)? {
						self.dealloc_reg();
						args.push(r);
					} else {
						args.push(r);
					}
				}
				let ca = Box::new(CallArgs {
					store_in: store,
					func: self.map_string(name),
					args: args.into_boxed_slice(),
				});

				self.instr.push(match expr {
					Obj::Some(expr) => Instruction::Call(expr, ca),
					Obj::_Self => {
						if let Some(&func) = self.methods.get(name) {
							let mut args = Box::new([0; 16]);
							for (i, &a) in ca.args.into_iter().enumerate() {
								args[i] = a;
							}
							Instruction::CallSelf {
								store_in: ca.store_in,
								func,
								args,
							}
						} else {
							err!(line, column, UndefinedFunction, name);
						}
					}
					Obj::Env => Instruction::CallEnv { args: ca },
				});
				self.min_var_count = self.min_var_count.max(self.curr_var_count);
				self.curr_var_count = og_cvc;
				Ok(None)
			}
			Expression::Array { array, .. } => {
				let og_cvc = self.curr_var_count;
				let (array_reg, ret) = if let Some(r) = store {
					(r, None)
				} else {
					let r = self.curr_var_count;
					self.curr_var_count += 1;
					(r, Some(r))
				};
				self.instr
					.push(Instruction::NewArray(array_reg, array.len()));
				for (i, expr) in array.into_iter().enumerate() {
					let r = self.curr_var_count;
					self.curr_var_count += 1;
					let r = if let Some(e) = self.parse_expression(Some(r), expr)? {
						self.curr_var_count -= 1;
						e
					} else {
						r
					};
					self.update_min_vars();
					let i = self.add_const(V::new_integer(i as isize));
					self.instr.push(Instruction::SetIndex(r, array_reg, i));
				}
				self.curr_var_count = og_cvc;
				Ok(ret)
			}
			Expression::Dictionary { dictionary, .. } => {
				let og_cvc = self.curr_var_count;
				let (dict_reg, ret) = if let Some(r) = store {
					(r, None)
				} else {
					let r = self.curr_var_count;
					self.curr_var_count += 1;
					(r, Some(r))
				};
				self.instr
					.push(Instruction::NewDictionary(dict_reg, dictionary.len()));
				for (key_expr, val_expr) in dictionary {
					let (k, v) = (self.curr_var_count, self.curr_var_count + 1);
					self.curr_var_count += 2;
					let k = if let Some(e) = self.parse_expression(Some(k), key_expr)? {
						self.curr_var_count -= 1;
						e
					} else {
						k
					};
					let v = if let Some(e) = self.parse_expression(Some(v), val_expr)? {
						self.curr_var_count -= 1;
						e
					} else {
						v
					};
					self.update_min_vars();
					self.instr.push(Instruction::SetIndex(v, dict_reg, k));
				}
				self.curr_var_count = og_cvc;
				Ok(ret)
			}
		}
	}

	fn add_const(&mut self, var: V) -> u8 {
		let key = Constant::from_variant(var).expect("Failed to convert Variant to Constant");
		match self.const_map.entry(key) {
			Entry::Vacant(e) => {
				self.consts.push(e.key().clone().into_variant());
				let r = u8::MAX - self.consts.len() as u8 + 1;
				e.insert(r);
				r
			}
			Entry::Occupied(e) => *e.get(),
		}
	}

	fn get_const(&self, reg: u8) -> Option<V> {
		// TODO add a reverse map
		for (k, &v) in self.const_map.iter() {
			if v == reg {
				return Some(k.clone().into_variant());
			}
		}
		None
	}

	fn map_string(&mut self, string: impl Into<Rc<str>> + Deref<Target = str>) -> Rc<str> {
		if let Some(string) = self.string_map.get(&*string) {
			string.clone()
		} else {
			let string: Rc<str> = string.into();
			self.string_map.insert(string.clone());
			string
		}
	}

	fn update_min_vars(&mut self) {
		self.min_var_count = self.min_var_count.max(self.curr_var_count);
	}

	fn alloc_reg(&mut self, line: u32, column: u32) -> Result<u8, ByteCodeError<'s>> {
		let r = self.curr_var_count;
		self.curr_var_count = self.curr_var_count.checked_add(1).ok_or_else(|| {
			ByteCodeError::new(line, column, ByteCodeErrorType::TooManyRegisters())
		})?;
		Ok(r)
	}

	fn dealloc_reg(&mut self) {
		self.curr_var_count -= 1;
	}
}

impl Constant {
	fn from_variant<V>(var: V) -> Result<Self, ()>
	where
		V: VariantType,
	{
		Ok(match var.as_bool() {
			Ok(v) => Self::Bool(v),
			Err(v) => match v.as_integer() {
				Ok(v) => Self::Int(v),
				Err(v) => match v.as_real() {
					Ok(v) => Self::Real(v),
					Err(_) => match var.into_string() {
						Ok(v) => Self::Str(v),
						Err(_) => return Err(()),
					},
				},
			},
		})
	}

	fn into_variant<V>(self) -> V
	where
		V: VariantType,
	{
		match self {
			Self::Bool(b) => V::new_bool(b),
			Self::Int(i) => V::new_integer(i),
			Self::Real(r) => V::new_real(r),
			Self::Str(s) => V::new_string(s),
		}
	}
}

impl hash::Hash for Constant {
	fn hash<H>(&self, h: &mut H)
	where
		H: hash::Hasher,
	{
		match self {
			Self::Bool(n) => h.write_u8(if *n { 1 } else { 0 }),
			Self::Int(n) => h.write_isize(*n),
			Self::Str(n) => h.write(n.as_bytes()),
			Self::Real(n) => {
				if n.is_nan() {
					h.write_u64(u64::MAX);
				} else {
					h.write(&n.to_ne_bytes());
				}
			}
		}
	}
}

impl PartialEq for Constant {
	fn eq(&self, rhs: &Self) -> bool {
		match (self, rhs) {
			(Self::Bool(a), Self::Bool(b)) => a == b,
			(Self::Int(a), Self::Int(b)) => a == b,
			(Self::Str(a), Self::Str(b)) => a == b,
			(Self::Real(a), Self::Real(b)) => (a.is_nan() && b.is_nan()) || (a == b),
			_ => false,
		}
	}
}

impl Eq for Constant {}

impl<'a> ByteCodeError<'a> {
	fn new(line: u32, column: u32, error: ByteCodeErrorType<'a>) -> Self {
		Self {
			line,
			column,
			error,
		}
	}
}

impl fmt::Display for ByteCodeError<'_> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		use fmt::Write;
		let mut w = |s, v: &str| {
			f.write_str(s)?;
			if !v.is_empty() {
				f.write_str(" '")?;
				f.write_str(v)?;
				f.write_char('\'')?;
			}
			Ok(())
		};
		match &self.error {
			ByteCodeErrorType::UndefinedVariable(v) => w("Undefined variable", v),
			ByteCodeErrorType::DuplicateVariable(v) => w("Duplicate variable", v),
			ByteCodeErrorType::DuplicateParameter(v) => w("Duplicate parameter", v),
			ByteCodeErrorType::UnexpectedBreak() => w("Unexpected ", "break"),
			ByteCodeErrorType::UnexpectedContinue() => w("Unexpected ", "continue"),
			&ByteCodeErrorType::TooManyRegisters() => {
				w("Too many registers allocated (use less variables!)", "")
			}
			ByteCodeErrorType::Unsupported(v) => w(v, ""),
			ByteCodeErrorType::UndefinedFunction(v) => w("Undefined function", v),
			ByteCodeErrorType::CantAssign(v) => w("Can't assign to", v),
		}
	}
}
