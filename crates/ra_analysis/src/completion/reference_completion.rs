use rustc_hash::{FxHashSet};
use ra_editor::find_node_at_offset;
use ra_syntax::{
    algo::visit::{visitor, Visitor},
    SourceFileNode, AstNode,
    ast::{self, LoopBodyOwner},
    SyntaxKind::*,
};
use hir::{
    self,
    FnScopes, Def, Path
};

use crate::{
    db::RootDatabase,
    completion::{CompletionItem, Completions},
    Cancelable
};

pub(super) fn completions(
    acc: &mut Completions,
    db: &RootDatabase,
    module: &hir::Module,
    file: &SourceFileNode,
    name_ref: ast::NameRef,
) -> Cancelable<()> {
    let kind = match classify_name_ref(name_ref) {
        Some(it) => it,
        None => return Ok(()),
    };

    match kind {
        NameRefKind::LocalRef { enclosing_fn } => {
            if let Some(fn_def) = enclosing_fn {
                let scopes = FnScopes::new(fn_def);
                complete_fn(name_ref, &scopes, acc);
                complete_expr_keywords(&file, fn_def, name_ref, acc);
                complete_expr_snippets(acc);
            }

            let module_scope = module.scope(db)?;
            module_scope
                .entries()
                .filter(|(_name, res)| {
                    // Don't expose this item
                    match res.import {
                        None => true,
                        Some(import) => {
                            let range = import.range(db, module.source().file_id());
                            !range.is_subrange(&name_ref.syntax().range())
                        }
                    }
                })
                .for_each(|(name, _res)| CompletionItem::new(name.to_string()).add_to(acc));
        }
        NameRefKind::Path(path) => complete_path(acc, db, module, path)?,
        NameRefKind::BareIdentInMod => {
            let name_range = name_ref.syntax().range();
            let top_node = name_ref
                .syntax()
                .ancestors()
                .take_while(|it| it.range() == name_range)
                .last()
                .unwrap();
            match top_node.parent().map(|it| it.kind()) {
                Some(SOURCE_FILE) | Some(ITEM_LIST) => complete_mod_item_snippets(acc),
                _ => (),
            }
        }
    }
    Ok(())
}

enum NameRefKind<'a> {
    /// NameRef is a part of single-segment path, for example, a refernece to a
    /// local variable.
    LocalRef {
        enclosing_fn: Option<ast::FnDef<'a>>,
    },
    /// NameRef is the last segment in some path
    Path(Path),
    /// NameRef is bare identifier at the module's root.
    /// Used for keyword completion
    BareIdentInMod,
}

fn classify_name_ref(name_ref: ast::NameRef) -> Option<NameRefKind> {
    let name_range = name_ref.syntax().range();
    let top_node = name_ref
        .syntax()
        .ancestors()
        .take_while(|it| it.range() == name_range)
        .last()
        .unwrap();
    match top_node.parent().map(|it| it.kind()) {
        Some(SOURCE_FILE) | Some(ITEM_LIST) => return Some(NameRefKind::BareIdentInMod),
        _ => (),
    }

    let parent = name_ref.syntax().parent()?;
    if let Some(segment) = ast::PathSegment::cast(parent) {
        let path = segment.parent_path();
        if let Some(path) = Path::from_ast(path) {
            if !path.is_ident() {
                return Some(NameRefKind::Path(path));
            }
        }
        if path.qualifier().is_none() {
            let enclosing_fn = name_ref
                .syntax()
                .ancestors()
                .take_while(|it| it.kind() != SOURCE_FILE && it.kind() != MODULE)
                .find_map(ast::FnDef::cast);
            return Some(NameRefKind::LocalRef { enclosing_fn });
        }
    }
    None
}

fn complete_fn(name_ref: ast::NameRef, scopes: &FnScopes, acc: &mut Completions) {
    let mut shadowed = FxHashSet::default();
    scopes
        .scope_chain(name_ref.syntax())
        .flat_map(|scope| scopes.entries(scope).iter())
        .filter(|entry| shadowed.insert(entry.name()))
        .for_each(|entry| CompletionItem::new(entry.name().to_string()).add_to(acc));
    if scopes.self_param.is_some() {
        CompletionItem::new("self").add_to(acc);
    }
}

fn complete_path(
    acc: &mut Completions,
    db: &RootDatabase,
    module: &hir::Module,
    mut path: Path,
) -> Cancelable<()> {
    if path.segments.is_empty() {
        return Ok(());
    }
    path.segments.pop();
    let def_id = match module.resolve_path(db, path)? {
        None => return Ok(()),
        Some(it) => it,
    };
    let target_module = match def_id.resolve(db)? {
        Def::Module(it) => it,
        _ => return Ok(()),
    };
    let module_scope = target_module.scope(db)?;
    module_scope
        .entries()
        .for_each(|(name, _res)| CompletionItem::new(name.to_string()).add_to(acc));
    Ok(())
}

fn complete_mod_item_snippets(acc: &mut Completions) {
    CompletionItem::new("Test function")
        .lookup_by("tfn")
        .snippet(
            "\
#[test]
fn ${1:feature}() {
    $0
}",
        )
        .add_to(acc);
    CompletionItem::new("pub(crate)")
        .snippet("pub(crate) $0")
        .add_to(acc);
}

fn complete_expr_keywords(
    file: &SourceFileNode,
    fn_def: ast::FnDef,
    name_ref: ast::NameRef,
    acc: &mut Completions,
) {
    acc.add(keyword("if", "if $0 {}"));
    acc.add(keyword("match", "match $0 {}"));
    acc.add(keyword("while", "while $0 {}"));
    acc.add(keyword("loop", "loop {$0}"));

    if let Some(off) = name_ref.syntax().range().start().checked_sub(2.into()) {
        if let Some(if_expr) = find_node_at_offset::<ast::IfExpr>(file.syntax(), off) {
            if if_expr.syntax().range().end() < name_ref.syntax().range().start() {
                acc.add(keyword("else", "else {$0}"));
                acc.add(keyword("else if", "else if $0 {}"));
            }
        }
    }
    if is_in_loop_body(name_ref) {
        acc.add(keyword("continue", "continue"));
        acc.add(keyword("break", "break"));
    }
    acc.add_all(complete_return(fn_def, name_ref));
}

fn is_in_loop_body(name_ref: ast::NameRef) -> bool {
    for node in name_ref.syntax().ancestors() {
        if node.kind() == FN_DEF || node.kind() == LAMBDA_EXPR {
            break;
        }
        let loop_body = visitor()
            .visit::<ast::ForExpr, _>(LoopBodyOwner::loop_body)
            .visit::<ast::WhileExpr, _>(LoopBodyOwner::loop_body)
            .visit::<ast::LoopExpr, _>(LoopBodyOwner::loop_body)
            .accept(node);
        if let Some(Some(body)) = loop_body {
            if name_ref
                .syntax()
                .range()
                .is_subrange(&body.syntax().range())
            {
                return true;
            }
        }
    }
    false
}

fn complete_return(fn_def: ast::FnDef, name_ref: ast::NameRef) -> Option<CompletionItem> {
    // let is_last_in_block = name_ref.syntax().ancestors().filter_map(ast::Expr::cast)
    //     .next()
    //     .and_then(|it| it.syntax().parent())
    //     .and_then(ast::Block::cast)
    //     .is_some();

    // if is_last_in_block {
    //     return None;
    // }

    let is_stmt = match name_ref
        .syntax()
        .ancestors()
        .filter_map(ast::ExprStmt::cast)
        .next()
    {
        None => false,
        Some(expr_stmt) => expr_stmt.syntax().range() == name_ref.syntax().range(),
    };
    let snip = match (is_stmt, fn_def.ret_type().is_some()) {
        (true, true) => "return $0;",
        (true, false) => "return;",
        (false, true) => "return $0",
        (false, false) => "return",
    };
    Some(keyword("return", snip))
}

fn keyword(kw: &str, snippet: &str) -> CompletionItem {
    CompletionItem::new(kw).snippet(snippet).build()
}

fn complete_expr_snippets(acc: &mut Completions) {
    CompletionItem::new("pd")
        .snippet("eprintln!(\"$0 = {:?}\", $0);")
        .add_to(acc);
    CompletionItem::new("ppd")
        .snippet("eprintln!(\"$0 = {:#?}\", $0);")
        .add_to(acc);
}
