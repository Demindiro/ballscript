// Copyright (C) 2021  David Hoppenbrouwers
//
// This file is licensed under the MIT license. See script/LICENSE for details.

#[cfg(not(feature = "std"))]
use crate::std_types::*;
use crate::util;

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Op {
	Add,
	Sub,
	Mul,
	Div,
	Rem,
	And,
	Or,
	Xor,
	Not,
	AndThen,
	OrElse,
	Eq,
	Neq,
	LessEq,
	GreaterEq,
	Less,
	Greater,
	ShiftLeft,
	ShiftRight,
	Access,
	Index,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum AssignOp {
	None,
	Add,
	Sub,
	Mul,
	Div,
	Rem,
	And,
	Or,
	Xor,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token<'src> {
	Number(&'src str),
	Name(&'src str),
	String(util::Str<'src>),
	Var,
	BracketRoundOpen,
	BracketRoundClose,
	BracketSquareOpen,
	BracketSquareClose,
	BracketCurlyOpen,
	BracketCurlyClose,
	Op(Op),
	Assign(AssignOp),
	If,
	Elif,
	Else,
	While,
	For,
	In,
	Fn,
	Indent(u8),
	Return,
	Comma,
	Pass,
	Colon,
	Break,
	Continue,
	As,
	Is,
	Try,
	Catch,
	True,
	False,
	To,
	Step,
	_Self,
	Env,
	Int,
	Real,
	Str,
}

#[derive(Debug, PartialEq)]
pub enum TokenError {
	Empty,
	UnterminatedString,
	InvalidAssignOp,
	SpaceInIndent,
	IndentationOverflow,
	InvalidEscapeSequence,
}

#[derive(Debug)]
pub(crate) struct TokenStream<'src> {
	tokens: Vec<(Token<'src>, u32, u32)>,
	current_index: usize,
}

#[derive(Debug, PartialEq)]
pub(crate) enum TokenStreamError {
	TokenError(TokenError),
}

impl Op {
	fn precedence(&self) -> i8 {
		use Op::*;
		match *self {
			Access => 13,
			Index => 12,
			Not => 11,
			Mul | Div | Rem => 10,
			Add | Sub => 9,
			ShiftRight | ShiftLeft => 8,
			And => 7,
			Xor => 6,
			Or => 5,
			Less | Greater | LessEq | GreaterEq => 4,
			Eq | Neq => 3,
			AndThen | OrElse => 2,
		}
	}
}

impl PartialOrd for Op {
	fn partial_cmp(&self, rhs: &Self) -> Option<core::cmp::Ordering> {
		self.precedence().partial_cmp(&rhs.precedence())
	}
}

impl Token<'_> {
	const OPERATORS: &'static str = "=+-*/%&|^!<>.";
	const BRACKETS: &'static str = "()[]{}";

	fn parse(source: &str, start_of_file: bool) -> Result<(Token, u32), TokenError> {
		let mut chars = source.char_indices().map(|(i, c)| (i as u32, c)).peekable();
		while let Some((start, c)) = chars.next() {
			return match c {
				'#' => {
					while chars.peek() != None && chars.peek().map(|v| v.1) != Some('\n') {
						chars.next();
					}
					continue;
				}
				' ' | '\t' if (c != '\t' || !start_of_file) => continue,
				'\n' | '\t' => {
					let s = if c == '\t' { 1 } else { 0 };
					for i in s.. {
						return match chars.next() {
							Some((_, '\t')) => continue,
							Some((_, ' ')) => Err(TokenError::SpaceInIndent),
							Some(_) => Ok((Token::Indent(i), start + (i + 1 - s) as u32)),
							None => Err(TokenError::Empty),
						};
					}
					return Err(TokenError::IndentationOverflow);
				}
				// Just ignore it, I can't be arsed with Windows legacy crap
				'\r' => continue,
				'(' => Ok((Token::BracketRoundOpen, start + 1)),
				')' => Ok((Token::BracketRoundClose, start + 1)),
				'[' => Ok((Token::BracketSquareOpen, start + 1)),
				']' => Ok((Token::BracketSquareClose, start + 1)),
				'{' => Ok((Token::BracketCurlyOpen, start + 1)),
				'}' => Ok((Token::BracketCurlyClose, start + 1)),
				',' => Ok((Token::Comma, start + 1)),
				':' => Ok((Token::Colon, start + 1)),
				'"' => {
					let mut start = start as usize + 1;
					let mut s = String::new();
					loop {
						if let Some((i, c)) = chars.next() {
							if c == '"' {
								let s = if s.is_empty() {
									util::Str::Slice(&source[start..i as usize])
								} else {
									s.push_str(&source[start..i as usize]);
									util::Str::Alloc(s.into())
								};
								break Ok((Token::String(s), i + 1));
							} else if c == '\\' {
								s.push_str(&source[start..i as usize]);
								start = i as usize + 2;
								s.push(match chars.next().map(|(_, c)| c) {
									Some('a') => '\x07',
									Some('b') => '\x08',
									Some('e') => '\x1b',
									Some('f') => '\x0f',
									Some('n') => '\n',
									Some('r') => '\r',
									Some('t') => '\t',
									Some('v') => '\x0b',
									Some('\\') => '\\',
									Some('\'') => '\'',
									Some('"') => '"',
									Some(a) if ('0'..='7').contains(&a) => {
										let a = a.to_digit(8).unwrap();
										let b = chars
											.next()
											.and_then(|(_, c)| c.to_digit(8))
											.ok_or(TokenError::InvalidEscapeSequence)?;
										let c = chars
											.next()
											.and_then(|(_, c)| c.to_digit(8))
											.ok_or(TokenError::InvalidEscapeSequence)?;
										start += 2;
										let n = a << 6 | b << 3 | c;
										char::from_u32(n)
											.ok_or(TokenError::InvalidEscapeSequence)?
									}
									Some('x') => {
										let a = chars
											.next()
											.and_then(|(_, c)| c.to_digit(16))
											.ok_or(TokenError::InvalidEscapeSequence)?;
										let b = chars
											.next()
											.and_then(|(_, c)| c.to_digit(16))
											.ok_or(TokenError::InvalidEscapeSequence)?;
										start += 2;
										let n = a << 4 | b;
										char::from_u32(n)
											.ok_or(TokenError::InvalidEscapeSequence)?
									}
									Some('u') => {
										let mut n = 0;
										for _ in 0..4 {
											let a = chars
												.next()
												.and_then(|(_, c)| c.to_digit(16))
												.ok_or(TokenError::InvalidEscapeSequence)?;
											n = (n << 4) | a;
											start += 1;
										}
										char::from_u32(n)
											.ok_or(TokenError::InvalidEscapeSequence)?
									}
									Some('U') => {
										let mut n = 0;
										for _ in 0..8 {
											let (_, a) = chars
												.next()
												.ok_or(TokenError::InvalidEscapeSequence)?;
											let a = a
												.to_digit(16)
												.ok_or(TokenError::InvalidEscapeSequence)?;
											n = (n << 4) | a;
											start += 1;
										}
										char::from_u32(n)
											.ok_or(TokenError::InvalidEscapeSequence)?
									}
									Some(_) => return Err(TokenError::InvalidEscapeSequence),
									None => return Err(TokenError::UnterminatedString),
								})
							}
						} else {
							break Err(TokenError::UnterminatedString);
						}
					}
				}
				_ if Self::OPERATORS.contains(c) => {
					if let Some(&(i, n)) = chars.peek() {
						if n == '=' {
							let i = i + 1;
							return match c {
								'+' => Ok((Token::Assign(AssignOp::Add), i)),
								'-' => Ok((Token::Assign(AssignOp::Sub), i)),
								'*' => Ok((Token::Assign(AssignOp::Mul), i)),
								'/' => Ok((Token::Assign(AssignOp::Div), i)),
								'%' => Ok((Token::Assign(AssignOp::Rem), i)),
								'&' => Ok((Token::Assign(AssignOp::And), i)),
								'|' => Ok((Token::Assign(AssignOp::Or), i)),
								'^' => Ok((Token::Assign(AssignOp::Xor), i)),
								'=' => Ok((Token::Op(Op::Eq), i)),
								'!' => Ok((Token::Op(Op::Neq), i)),
								'<' => Ok((Token::Op(Op::LessEq), i)),
								'>' => Ok((Token::Op(Op::GreaterEq), i)),
								_ => Err(TokenError::InvalidAssignOp),
							};
						}
					}
					let cn = chars.peek().map(|v| v.1);
					let either2 = |c, y, n| {
						if cn == Some(c) {
							(Token::Op(y), start + 2)
						} else {
							(Token::Op(n), start + 1)
						}
					};
					let either3 = |c1, y1, c2, y2, n| {
						if cn == Some(c1) {
							(Token::Op(y1), start + 2)
						} else {
							either2(c2, y2, n)
						}
					};
					let i = start + 1;
					match c {
						'+' => Ok((Token::Op(Op::Add), i)),
						'-' => Ok((Token::Op(Op::Sub), i)),
						'*' => Ok((Token::Op(Op::Mul), i)),
						'/' => Ok((Token::Op(Op::Div), i)),
						'%' => Ok((Token::Op(Op::Rem), i)),
						'&' => Ok(either2('&', Op::AndThen, Op::And)),
						'|' => Ok(either2('|', Op::OrElse, Op::Or)),
						'^' => Ok((Token::Op(Op::Xor), i)),
						'=' => Ok((Token::Assign(AssignOp::None), i)),
						'<' => Ok(either3('<', Op::ShiftLeft, '=', Op::LessEq, Op::Less)),
						'>' => Ok(either3(
							'>',
							Op::ShiftRight,
							'=',
							Op::GreaterEq,
							Op::Greater,
						)),
						'!' => Ok((Token::Op(Op::Not), i)),
						'.' => Ok((Token::Op(Op::Access), i)),
						c => unreachable!("operator '{}' not covered", c),
					}
				}
				_ if c.is_digit(10) => {
					let start = start as usize;
					let mut dot_encountered = false;
					let mut prev_was_dot = false;
					loop {
						if let Some((i, c)) = chars.next() {
							if !c.is_alphanumeric() && c != '_' {
								if dot_encountered || c != '.' {
									let i = if prev_was_dot { i - 1 } else { i };
									let s = &source[start..i as usize];
									break Ok((Token::Number(s), i));
								} else {
									dot_encountered = true;
									prev_was_dot = true;
								}
							} else {
								prev_was_dot = false;
							}
						} else {
							let s = &source[start..];
							break Ok((Token::Number(s), source.len() as u32));
						}
					}
				}
				_ => {
					let start = start as usize;
					let (s, i) = loop {
						if let Some((i, c)) = chars.next() {
							if c.is_whitespace()
								|| Self::OPERATORS.contains(c) || Self::BRACKETS.contains(c)
								|| c == ','
							{
								break (&source[start..i as usize], i);
							}
						} else {
							break (&source[start..], source.len() as u32);
						}
					};
					Ok((
						match s {
							"if" => Token::If,
							"else" => Token::Else,
							"elif" => Token::Elif,
							"while" => Token::While,
							"for" => Token::For,
							"in" => Token::In,
							"var" => Token::Var,
							"fn" => Token::Fn,
							"return" => Token::Return,
							"pass" => Token::Pass,
							"continue" => Token::Continue,
							"break" => Token::Break,
							"as" => Token::As,
							"is" => Token::Is,
							"try" => Token::Try,
							"catch" => Token::Catch,
							"true" => Token::True,
							"false" => Token::False,
							"to" => Token::To,
							"step" => Token::Step,
							"int" => Token::Int,
							"real" => Token::Real,
							"str" => Token::Str,
							"env" => Token::Env,
							"self" => Token::_Self,
							_ => Token::Name(s),
						},
						i as u32,
					))
				}
			};
		}
		Err(TokenError::Empty)
	}
}

impl<'src> TokenStream<'src> {
	pub(crate) fn parse(mut source: &'src str) -> Result<Self, TokenStreamError> {
		let mut line = 0;
		let mut column = 0;
		let mut tokens = Vec::new();
		let mut start = true;
		loop {
			match Token::parse(source, start) {
				Ok((tk, len)) => {
					let prev_col = if let Token::Indent(i) = tk {
						line += 1;
						column = i as u32;
						0
					} else {
						let c = column;
						column += len;
						c
					};
					tokens.push((tk, line, prev_col));
					source = &source[len as usize..];
					start = false;
				}
				Err(e) => {
					break if let TokenError::Empty = e {
						Self::remove_redundant(&mut tokens);
						Ok(Self {
							tokens,
							current_index: 0,
						})
					} else {
						Err(TokenStreamError::TokenError(e))
					}
				}
			}
		}
	}

	/// Returns the next token and advances the iterator
	pub(crate) fn next(&mut self) -> Option<Token<'src>> {
		if self.current_index < self.tokens.len() {
			self.current_index += 1;
			Some(self.tokens[self.current_index - 1].clone().0)
		} else {
			None
		}
	}

	/// Rewinds the iterator by one token
	pub(crate) fn prev(&mut self) {
		if self.current_index > 0 {
			self.current_index -= 1;
		}
	}

	/// Returns the line and column of the current token
	pub(crate) fn position(&self) -> (u32, u32) {
		let e = &self.tokens[self.current_index - 1];
		(e.1, e.2)
	}

	/// Removes redundant tokens, such as multiple Indents in a row. It also shrinks the vec
	fn remove_redundant(tokens: &mut Vec<(Token, u32, u32)>) {
		// Remove trailing newlines
		while let Some((Token::Indent(_), ..)) = tokens.last() {
			tokens.pop().unwrap();
		}
		let mut prev_was_indent = false;
		// Remove double indents
		for i in (0..tokens.len()).rev() {
			match tokens[i] {
				(Token::Indent(_), ..) => {
					if prev_was_indent {
						tokens.remove(i);
					}
					prev_was_indent = true;
				}
				_ => prev_was_indent = false,
			}
		}
		tokens.shrink_to_fit();
	}
}

/// Converts [`AssignOp`] into [`Op`] if applicable, otherwise it returns [`None`].
impl From<AssignOp> for Option<Op> {
	fn from(op: AssignOp) -> Self {
		Some(match op {
			AssignOp::None => return None,
			AssignOp::Add => Op::Add,
			AssignOp::Sub => Op::Sub,
			AssignOp::Mul => Op::Mul,
			AssignOp::Div => Op::Div,
			AssignOp::Rem => Op::Rem,
			AssignOp::And => Op::And,
			AssignOp::Or => Op::Or,
			AssignOp::Xor => Op::Xor,
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	mod token {
		use super::*;

		#[test]
		fn empty() {
			assert_eq!(Token::parse("", true), Err(TokenError::Empty));
			assert_eq!(
				Token::parse("# This is a comment", true),
				Err(TokenError::Empty)
			);
		}

		#[test]
		fn number() {
			assert_eq!(Token::parse("0", true), Ok((Token::Number("0"), 1)));
			assert_eq!(
				Token::parse("42_i32", true),
				Ok((Token::Number("42_i32"), 6))
			);
			assert_eq!(
				Token::parse("0b10101", true),
				Ok((Token::Number("0b10101"), 7))
			);
			assert_eq!(Token::parse("13.37", true), Ok((Token::Number("13.37"), 5)));
		}

		#[test]
		fn string() {
			assert_eq!(
				Token::parse("\"foo bar 42\"", true),
				Ok((Token::String("foo bar 42"), 12))
			);
		}

		#[test]
		fn control() {
			assert_eq!(Token::parse("if", true), Ok((Token::If, 2)));
			assert_eq!(Token::parse("else", true), Ok((Token::Else, 4)));
			assert_eq!(Token::parse("elif", true), Ok((Token::Elif, 4)));
			assert_eq!(Token::parse("while", true), Ok((Token::While, 5)));
			assert_eq!(Token::parse("for", true), Ok((Token::For, 3)));
			assert_eq!(Token::parse("in", true), Ok((Token::In, 2)));
			assert_eq!(Token::parse("return", true), Ok((Token::Return, 6)));
		}

		#[test]
		fn brackets() {
			assert_eq!(Token::parse("(", true), Ok((Token::BracketRoundOpen, 1)));
			assert_eq!(Token::parse(")", true), Ok((Token::BracketRoundClose, 1)));
			assert_eq!(Token::parse("[", true), Ok((Token::BracketSquareOpen, 1)));
			assert_eq!(Token::parse("]", true), Ok((Token::BracketSquareClose, 1)));
			assert_eq!(Token::parse("{", true), Ok((Token::BracketCurlyOpen, 1)));
			assert_eq!(Token::parse("}", true), Ok((Token::BracketCurlyClose, 1)));
		}

		#[test]
		fn op() {
			assert_eq!(Token::parse("+", true), Ok((Token::Op(Op::Add), 1)));
			assert_eq!(Token::parse("-", true), Ok((Token::Op(Op::Sub), 1)));
			assert_eq!(Token::parse("*", true), Ok((Token::Op(Op::Mul), 1)));
			assert_eq!(Token::parse("/", true), Ok((Token::Op(Op::Div), 1)));
			assert_eq!(Token::parse("%", true), Ok((Token::Op(Op::Rem), 1)));
			assert_eq!(Token::parse("&", true), Ok((Token::Op(Op::And), 1)));
			assert_eq!(Token::parse("|", true), Ok((Token::Op(Op::Or), 1)));
			assert_eq!(Token::parse("^", true), Ok((Token::Op(Op::Xor), 1)));
			assert_eq!(Token::parse("!", true), Ok((Token::Op(Op::Not), 1)));
			assert_eq!(Token::parse("<", true), Ok((Token::Op(Op::Less), 1)));
			assert_eq!(Token::parse(">", true), Ok((Token::Op(Op::Greater), 1)));
			assert_eq!(Token::parse("!=", true), Ok((Token::Op(Op::Neq), 2)));
			assert_eq!(Token::parse("<=", true), Ok((Token::Op(Op::LessEq), 2)));
			assert_eq!(Token::parse(">=", true), Ok((Token::Op(Op::GreaterEq), 2)));
		}

		#[test]
		fn assign_op() {
			assert_eq!(
				Token::parse("=", true),
				Ok((Token::Assign(AssignOp::None), 1))
			);
			assert_eq!(
				Token::parse("+=", true),
				Ok((Token::Assign(AssignOp::Add), 2))
			);
			assert_eq!(
				Token::parse("-=", true),
				Ok((Token::Assign(AssignOp::Sub), 2))
			);
			assert_eq!(
				Token::parse("*=", true),
				Ok((Token::Assign(AssignOp::Mul), 2))
			);
			assert_eq!(
				Token::parse("/=", true),
				Ok((Token::Assign(AssignOp::Div), 2))
			);
			assert_eq!(
				Token::parse("%=", true),
				Ok((Token::Assign(AssignOp::Rem), 2))
			);
			assert_eq!(
				Token::parse("&=", true),
				Ok((Token::Assign(AssignOp::And), 2))
			);
			assert_eq!(
				Token::parse("|=", true),
				Ok((Token::Assign(AssignOp::Or), 2))
			);
			assert_eq!(
				Token::parse("^=", true),
				Ok((Token::Assign(AssignOp::Xor), 2))
			);
		}

		#[test]
		fn declare() {
			assert_eq!(Token::parse("var foo", true), Ok((Token::Var, 3)));
		}

		#[test]
		fn name() {
			assert_eq!(Token::parse("foo", true), Ok((Token::Name("foo"), 3)));
			assert_eq!(Token::parse("_4343", true), Ok((Token::Name("_4343"), 5)));
			assert_eq!(
				Token::parse("hunter2", true),
				Ok((Token::Name("hunter2"), 7))
			);
		}

		#[test]
		fn other() {
			assert_eq!(Token::parse(" ", true), Err(TokenError::Empty));
			assert_eq!(Token::parse("\n", true), Err(TokenError::Empty));
			assert_eq!(Token::parse("\r\n", true), Err(TokenError::Empty));
			//assert_eq!(Token::parse("\t\n\t", true), Err(TokenError::Empty));
			assert_eq!(
				Token::parse("\t\tblah blah", true),
				Ok((Token::Indent(2), 2))
			);
			assert_eq!(
				Token::parse(" blah blah", true),
				Ok((Token::Name("blah"), 5))
			);
			assert_eq!(Token::parse(",", true), Ok((Token::Comma, 1)));
			assert_eq!(Token::parse("pass", true), Ok((Token::Pass, 4)));
		}
	}

	mod stream {
		use super::*;

		#[test]
		fn next_prev() {
			let src = "fn";
			let mut s = TokenStream::parse(src).expect("Failed to parse source");
			assert_eq!(s.prev(), false);
			assert_eq!(s.next(), Some(Token::Fn));
			assert_eq!(s.next(), None);
			assert_eq!(s.prev(), true);
			assert_eq!(s.prev(), false);
		}

		#[test]
		fn hello_world() {
			let src = "fn main()\n\tprintln(\"Hello, world!\")";
			let mut s = TokenStream::parse(src).expect("Failed to parse source");
			assert_eq!(s.next(), Some(Token::Fn));
			assert_eq!(s.next(), Some(Token::Name("main")));
			assert_eq!(s.next(), Some(Token::BracketRoundOpen));
			assert_eq!(s.next(), Some(Token::BracketRoundClose));
			assert_eq!(s.next(), Some(Token::Indent(1)));
			assert_eq!(s.next(), Some(Token::Name("println")));
			assert_eq!(s.next(), Some(Token::BracketRoundOpen));
			assert_eq!(s.next(), Some(Token::String("Hello, world!")));
			assert_eq!(s.next(), Some(Token::BracketRoundClose));
			assert_eq!(s.next(), None);
		}

		#[test]
		fn vector_len() {
			let src = "fn vec2_len(x, y)\n\treturn x * x + y * y";
			let mut s = TokenStream::parse(src).expect("Failed to parse source");
			assert_eq!(s.next(), Some(Token::Fn));
			assert_eq!(s.next(), Some(Token::Name("vec2_len")));
			assert_eq!(s.next(), Some(Token::BracketRoundOpen));
			assert_eq!(s.next(), Some(Token::Name("x")));
			assert_eq!(s.next(), Some(Token::Comma));
			assert_eq!(s.next(), Some(Token::Name("y")));
			assert_eq!(s.next(), Some(Token::BracketRoundClose));
			assert_eq!(s.next(), Some(Token::Indent(1)));
			assert_eq!(s.next(), Some(Token::Return));
			assert_eq!(s.next(), Some(Token::Name("x")));
			assert_eq!(s.next(), Some(Token::Op(Op::Mul)));
			assert_eq!(s.next(), Some(Token::Name("x")));
			assert_eq!(s.next(), Some(Token::Op(Op::Add)));
			assert_eq!(s.next(), Some(Token::Name("y")));
			assert_eq!(s.next(), Some(Token::Op(Op::Mul)));
			assert_eq!(s.next(), Some(Token::Name("y")));
			assert_eq!(s.next(), None);
		}
	}
}
