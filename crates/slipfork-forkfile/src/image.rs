//! `image` and build-time `arg` declarations.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::parser::Parser;
use crate::span::Span;
use crate::token::TokenKind;

impl<'a> Parser<'a> {
    pub(crate) fn parse_image(&mut self, span: Span) -> Result<Image, Diagnostic> {
        let id_span = self.current_span()?;
        let id = self.expect_ident(
            "an image id",
            "write `image <id> <dir>` or `image <id> from <ref>`",
        )?;
        let (word, _) = self.raw_word().map_err(|_| {
            Diagnostic::new(
                id_span,
                "expected an image directory or `from <ref>`",
                "write `image <id> <dir>` or `image <id> from <ref>`",
            )
        })?;
        let source = if word == "from" {
            let (reference, _) = self.raw_word()?;
            ImageSource::From(reference)
        } else {
            ImageSource::Dir(word)
        };
        let body = self.parse_block(|parser| parser.parse_image_item())?;
        Ok(Image {
            id,
            id_span,
            source,
            body,
            span,
        })
    }

    fn parse_image_item(&mut self) -> Result<ImageItem, Diagnostic> {
        let token = self.bump()?;
        let span = token.span;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected an image property, found {}", other.describe()),
                    "use `service`, `env`, `arg`, `stage`, `use env`, `dockerfile`, `workdir`, `before-start`, or `on`",
                ));
            }
        };
        match name.as_str() {
            "dockerfile" => {
                let (path, _) = self.raw_word()?;
                Ok(ImageItem::Dockerfile(path))
            }
            "stage" => self
                .expect_ident("a stage name", "write `stage <name>`")
                .map(ImageItem::Stage),
            "workdir" => {
                let (path, _) = self.raw_word()?;
                Ok(ImageItem::Workdir(path))
            }
            "arg" => {
                let value = self.bump()?;
                let key_span = value.span;
                match value.kind {
                    TokenKind::LBrace => {
                        let args = self.parse_block_body(|parser| parser.parse_arg())?;
                        Ok(ImageItem::ArgBlock(ArgBlock { args, span }))
                    }
                    TokenKind::Ident(key) => {
                        self.expect_punct(TokenKind::Eq, "`=`", "write `arg KEY = <expr>`")?;
                        let value = self.parse_expr()?;
                        Ok(ImageItem::Arg(ArgDecl {
                            key,
                            key_span,
                            value,
                            span,
                        }))
                    }
                    other => Err(Diagnostic::new(
                        value.span,
                        format!(
                            "expected an argument name or `{{`, found {}",
                            other.describe()
                        ),
                        "write `arg KEY = <expr>` or `arg { KEY = <expr> }`",
                    )),
                }
            }
            "use" => {
                let keyword = self.expect_ident("`env`", "write `use env <group>`")?;
                if keyword != "env" {
                    return Err(Diagnostic::new(
                        span,
                        format!("expected `env` after `use`, found `{keyword}`"),
                        "write `use env <group>`",
                    ));
                }
                self.expect_ident("an env group", "write `use env <group>`")
                    .map(ImageItem::UseEnv)
            }
            "env" => {
                self.expect_punct(TokenKind::LBrace, "`{`", "write `env { KEY = <expr> }`")?;
                let entries = self.parse_block_body(|parser| parser.parse_env_item())?;
                Ok(ImageItem::Env(EnvBlock { entries, span }))
            }
            "before-start" => self.parse_hook(true, span).map(ImageItem::BeforeStart),
            "pre-deploy" => self.parse_hook(false, span).map(ImageItem::PreDeploy),
            "post-deploy" => self.parse_hook(false, span).map(ImageItem::PostDeploy),
            "failed-deploy" => self.parse_hook(false, span).map(ImageItem::FailedDeploy),
            "service" => self.parse_service(span).map(ImageItem::Service),
            "on" => self
                .parse_on_after_keyword(span, &mut |parser: &mut Self| parser.parse_image_item())
                .map(ImageItem::On),
            other => Err(Diagnostic::new(
                span,
                format!("unknown image property `{other}`"),
                "use `service`, `env`, `arg`, `stage`, `use env`, `dockerfile`, `workdir`, `before-start`, or `on`",
            )),
        }
    }

    fn parse_arg(&mut self) -> Result<ArgDecl, Diagnostic> {
        let span = self.current_span()?;
        let (key, key_span) =
            self.expect_ident_spanned("an argument name", "write `KEY = <expr>`")?;
        self.expect_punct(TokenKind::Eq, "`=`", "write `KEY = <expr>`")?;
        let value = self.parse_expr()?;
        Ok(ArgDecl {
            key,
            key_span,
            value,
            span,
        })
    }
}
