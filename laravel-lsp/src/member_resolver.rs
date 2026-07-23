//! Magic-member resolve + classify engine.
//!
//! M3 of the semantic-index plan. Given a member access — property-form
//! (`$user->email`) or call-form (`User::active()`, `$q->active()`) — resolve
//! the receiver to a class and classify the member against that class's
//! inheritance-resolved Laravel surfaces (scopes, accessors, relationships,
//! columns) plus dynamic finders.
//!
//! This module is the **engine**: pure functions over a [`ClassView`] (and,
//! for the orchestrator added later, the class-hierarchy index + a ClassView
//! cache). M3 ships the engine + fixtures; M4 wires it into the reverse
//! reference index and find-references.
//!
//! Classification is inheritance/trait resolved: the `declaring_fqcn` is the
//! class or trait that actually declares the member (via [`ClassView`]'s
//! `source_class` provenance), so a trait-shared scope keys once and downstream
//! rename/lens can attribute every inheriting model correctly.

use crate::class_hierarchy_index::ClassHierarchyIndex;
use crate::laravel_introspector::chain::{analyze, ClassView, LaravelClassKind};
use crate::laravel_introspector::model_metadata::pascal_to_snake;
use crate::parser::parse_php;
use crate::query_chain::flow;
use crate::query_chain::use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
use crate::salsa_impl::{Confidence, MagicMemberKind, MemberAccessReferenceData};
use crate::symbol_index::MagicMemberEntry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tree_sitter::Node;

/// Maps a class FQCN to the file that declares it — the only thing receiver
/// resolution needs from the class graph. Implemented by the actor-owned
/// [`ClassHierarchyIndex`] (used at query time) and by a plain
/// `HashMap<String, PathBuf>` snapshot (used by the parallel index-build pass,
/// which can't borrow the actor-owned index). Decoupling here means resolution
/// works the same on either.
pub trait ClassFileResolver {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf>;

    /// Resolve a container-binding key — the string in `app('key')` /
    /// `resolve('key')` — to the concrete class FQCN it was bound to, if any.
    ///
    /// Defaulted to `None` so implementors that don't model container bindings
    /// (the bare class index, the `HashMap<String, PathBuf>` test stub) need no
    /// changes; the binding-aware resolvers on the salsa and main sides override
    /// it with the parsed binding registry. This is the seam that lets the
    /// receiver resolver type `app('key')->member` without threading a second
    /// resolver argument through the whole call graph.
    fn binding_concrete(&self, _key: &str) -> Option<String> {
        None
    }

    /// The facade alias map — token (`Auth`) → facade FQCN
    /// (`Illuminate\Support\Facades\Auth`) — for resolving a facade static-call
    /// receiver to its real implementation.
    ///
    /// Defaulted to the built-in [`default_facade_aliases`] seed so implementors
    /// that don't model user aliases (the bare class index, test stubs) resolve
    /// the framework facades with no changes. The binding-aware resolvers on the
    /// salsa and main sides override it with the merged map (seed +
    /// `config/app.php` `aliases` + `bootstrap/app.php` `withAliases`), so a user
    /// alias for an existing token wins and new tokens are seen. Rides the
    /// resolver exactly as [`binding_concrete`](Self::binding_concrete) does — no
    /// extra argument threaded through the receiver-resolution call graph.
    ///
    /// Returns a [`Cow`] so the default yields an owned seed while overrides
    /// borrow their cached `Arc`-shared map without cloning the dozens of entries
    /// on every receiver resolution.
    fn facade_aliases(&self) -> std::borrow::Cow<'_, HashMap<String, String>> {
        std::borrow::Cow::Owned(crate::facade_resolver::default_facade_aliases())
    }

    /// Resolve a runtime-registered macro/mixin member — the
    /// `(receiver_fqcn, name)` pair where `receiver_fqcn` is the resolved
    /// Macroable host (`Illuminate\Support\Str`) and `name` is the called member
    /// (`uuid7`) — to its definition site `(file, 0-based line)`, if the
    /// project-wide macro registry has one.
    ///
    /// Defaulted to `None` so implementors that don't model macros (the bare
    /// class index, the `HashMap<String, PathBuf>` test stub) need no changes;
    /// the registry-aware resolvers on the salsa and main sides override it with
    /// the parsed macro registry. This is the classification surface that lets
    /// `Str::uuid7()` resolve when `uuid7` is registered in a provider — it rides
    /// the resolver exactly as [`binding_concrete`](Self::binding_concrete) does.
    fn macro_target(&self, _receiver_fqcn: &str, _name: &str) -> Option<(PathBuf, u32)> {
        None
    }

    /// Whether the macro registry holds *any* macro for `receiver_fqcn` — i.e.
    /// `receiver_fqcn` is a known Macroable host.
    ///
    /// The static-receiver arm uses this to yield an FQCN for a host the class
    /// index doesn't carry: the dominant Macroable hosts (`Illuminate\Support\Str`,
    /// `Arr`, `Request`) are vendor classes absent from the project index, so the
    /// plain `class_file`-gated resolution drops them and no macro on them would
    /// ever classify. A host with a registered macro is resolvable even without an
    /// indexed file; the per-member [`macro_target`](Self::macro_target) lookup in
    /// classification still gates which members actually resolve.
    ///
    /// Defaulted to `false` so non-registry implementors are unaffected.
    fn has_macro_host(&self, _receiver_fqcn: &str) -> bool {
        false
    }

    /// Classes that directly implement `interface_fqcn`.
    ///
    /// The contract→concrete fallback for helper / method-return chains: a
    /// method whose declared return type is an interface (`view()->make()`
    /// returns `Illuminate\Contracts\View\View`) can't classify against the
    /// contract's empty surface, so resolution falls back to the interface's
    /// concrete implementor(s). Rides the resolver exactly as
    /// [`binding_concrete`](Self::binding_concrete) does — no extra argument
    /// threaded through the receiver-resolution call graph.
    ///
    /// Defaulted to an empty slice so implementors that don't model the class
    /// hierarchy (the bare class index, the `HashMap<String, PathBuf>` test
    /// stub) need no changes; the [`ClassHierarchyIndex`]-backed resolvers
    /// override it with the real reverse-edge map.
    fn implementers_of(&self, _interface_fqcn: &str) -> Vec<String> {
        Vec::new()
    }
}

impl ClassFileResolver for ClassHierarchyIndex {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.get(fqcn).map(|node| node.file_path.clone())
    }
    fn implementers_of(&self, interface_fqcn: &str) -> Vec<String> {
        ClassHierarchyIndex::implementers_of(self, interface_fqcn).to_vec()
    }
}

impl ClassFileResolver for HashMap<String, PathBuf> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.get(fqcn).cloned()
    }
}

/// A [`ClassFileResolver`] backed by owned snapshots — the resolver the
/// out-of-actor magic-member build passes use. The `fqcn → file` snapshot
/// answers `class_file`; the `binding key → concrete FQCN` snapshot answers
/// `binding_concrete`, so `app('key')->member` resolves during indexing exactly
/// as it does for the live, in-actor query path. Both maps are `Arc`-shared, so
/// the per-file build tasks clone the resolver cheaply.
pub struct SnapshotResolver {
    pub class_files: Arc<HashMap<String, PathBuf>>,
    pub bindings: Arc<HashMap<String, String>>,
    /// The merged facade alias map (token → facade FQCN), snapshotted from the
    /// actor alongside `bindings` so the build pass resolves facade receivers
    /// (`Auth::check()`) the same way the live query path does.
    pub facade_aliases: Arc<HashMap<String, String>>,
    /// The macro registry — `(receiver_fqcn, macro_name)` → `(decl_file,
    /// decl_line)` — snapshotted from the actor so the build pass classifies
    /// runtime-registered macro/mixin members the same way the live query path
    /// does.
    pub macros: Arc<HashMap<(String, String), (PathBuf, u32)>>,
    /// The interface→implementors reverse map — `interface FQCN` → directly
    /// implementing class FQCNs — snapshotted from the class-hierarchy index so
    /// the build pass resolves contract-returning helper / method-return chains
    /// (`view()->make()->render()`) to the concrete implementor the same way the
    /// live query path does.
    pub implementers: Arc<HashMap<String, Vec<String>>>,
}

impl ClassFileResolver for SnapshotResolver {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.class_files.get(fqcn).cloned()
    }
    fn binding_concrete(&self, key: &str) -> Option<String> {
        self.bindings.get(key).cloned()
    }
    fn facade_aliases(&self) -> std::borrow::Cow<'_, HashMap<String, String>> {
        std::borrow::Cow::Borrowed(&self.facade_aliases)
    }
    fn macro_target(&self, receiver_fqcn: &str, name: &str) -> Option<(PathBuf, u32)> {
        self.macros
            .get(&(receiver_fqcn.to_string(), name.to_string()))
            .cloned()
    }
    fn has_macro_host(&self, receiver_fqcn: &str) -> bool {
        self.macros.keys().any(|(host, _)| host == receiver_fqcn)
    }
    fn implementers_of(&self, interface_fqcn: &str) -> Vec<String> {
        self.implementers
            .get(interface_fqcn)
            .cloned()
            .unwrap_or_default()
    }
}

// `AccessForm` moved to `salsa_impl` (it now travels inside
// `MemberAccessReferenceData` through the pattern cache); re-exported here so
// the engine's callers keep their `member_resolver::AccessForm` paths.
pub use crate::salsa_impl::AccessForm;

/// The classification of a resolved member: which declaring class owns it
/// (inheritance/trait resolved) and what magic kind it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedMember {
    /// FQCN of the class/trait that declares the member.
    pub declaring_fqcn: String,
    pub kind: MagicMemberKind,
}

/// Classify `member` accessed via `form` against `view`'s resolved surfaces.
///
/// Returns `None` when the member matches nothing known on the class — this is
/// what prunes the M2 capture firehose: an arbitrary `$x->whatever` whose
/// receiver resolves to a class without a matching member is simply dropped.
///
/// **Precedence** (first match wins, mirroring how Eloquent's magic resolves):
/// - property read: accessor → relationship → column → plain property
/// - call: scope → dynamic finder → relationship → plain method
///
/// Collisions between these are rare in real models; the order is fixed and
/// documented so classification is deterministic.
pub fn classify_member(
    view: &ClassView,
    member: &str,
    form: AccessForm,
) -> Option<ClassifiedMember> {
    if form.is_call() {
        classify_call(view, member)
    } else {
        classify_property(view, member)
    }
}

fn classify_property(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    // Accessor — explicit `get*Attribute` / `Attribute`-returning method.
    // Shadows a raw column of the same name (Laravel returns the accessor).
    if let Some(a) = view.accessors.iter().find(|a| a.property_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: a.source_class.clone(),
            kind: MagicMemberKind::Accessor,
        });
    }
    // Relationship read as a property (`$user->posts` → Collection/Model).
    if let Some(r) = view.relationships.iter().find(|r| r.method_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: r.source_class.clone(),
            kind: MagicMemberKind::Relationship,
        });
    }
    // Many-to-many `->pivot` on a model declaring a custom pivot class
    // (`protected $pivotClass = MembershipPivot::class;`, issue #30 item 4).
    // Models without the override fall through — the default
    // `Relations\Pivot` is vendor territory, not worth a card or a goto.
    if member == "pivot" && view.kind == LaravelClassKind::Model {
        if let Some(pivot) = &view.pivot_class {
            return Some(ClassifiedMember {
                declaring_fqcn: pivot.clone(),
                kind: MagicMemberKind::Pivot,
            });
        }
    }
    // Database column surfaced as a model attribute.
    if view.column_surface.iter().any(|c| c.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: view.fqcn.clone(),
            kind: MagicMemberKind::Column,
        });
    }
    // Plain (non-magic) property declared somewhere in the hierarchy.
    if let Some(p) = view.all_properties.iter().find(|p| p.value.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: p.source_class.clone(),
            kind: MagicMemberKind::PlainMember,
        });
    }
    None
}

fn classify_call(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    // Local scope (`scopeActive` → `->active()` / `Model::active()`).
    if let Some(s) = view.scopes.iter().find(|s| s.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: s.source_class.clone(),
            kind: MagicMemberKind::Scope,
        });
    }
    // Dynamic finder (`User::whereEmail(...)`).
    if let Some(classified) = classify_dynamic_finder(view, member) {
        return Some(classified);
    }
    // Relationship called as a method (`$user->posts()` → Builder).
    if let Some(r) = view.relationships.iter().find(|r| r.method_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: r.source_class.clone(),
            kind: MagicMemberKind::Relationship,
        });
    }
    // Plain (non-magic) method declared somewhere in the hierarchy.
    if let Some(m) = view.all_methods.iter().find(|m| m.value.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: m.source_class.clone(),
            kind: MagicMemberKind::PlainMember,
        });
    }
    None
}

/// A fully resolved + classified member access: the inheritance-resolved
/// declaring class, the magic kind, and the confidence with which the
/// receiver was resolved. M4 maps this into a [`MagicMemberEntry`] for the
/// reverse reference index (it does not persist back into the per-file
/// `ParsedPatternsData` cache, whose scaffold fields stay the typed contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMemberAccess {
    pub declaring_fqcn: String,
    pub kind: MagicMemberKind,
    pub confidence: Confidence,
}

/// Per-FQCN [`ClassView`] memo so resolving a project's member-access firehose
/// analyzes each model file once, not once per access site. Caches misses too
/// (a `None`) so an unreadable / class-less file isn't re-analyzed repeatedly.
///
/// # Why interior mutability (`DashMap`) instead of `&mut HashMap`
///
/// The whole-project index build fans every referencing file out across
/// `spawn_blocking` workers. Before, each worker built its *own* cache, so a
/// class referenced from N files was analyzed N times — the O(n²) this type
/// exists to kill. To share **one** cache across all those parallel workers it
/// has to be `Send + Sync` and mutate through a shared `&` (a `&mut` can't be
/// held by many threads at once). [`dashmap::DashMap`] gives exactly that: a
/// sharded concurrent map whose `entry`/`get` take `&self`, so every method
/// here takes `&self` and the build wraps the cache in one `Arc` cloned into
/// each worker. Single-threaded callers (the live query paths) are unaffected —
/// they just pay a negligible shard lock per lookup.
///
/// `ClassView` and everything it owns is plain data (`Send + Sync`), so
/// `Arc<ClassView>` crosses the `spawn_blocking` boundary freely.
#[derive(Default)]
pub struct ClassViewCache {
    cache: dashmap::DashMap<String, Option<Arc<ClassView>>>,
    /// Instrumentation: cache hits and misses (a "miss" is a first-time build
    /// of an FQCN, hit or fail). The magic-build log and the once-per-FQCN
    /// regression test read these to prove the sharing actually collapses work.
    /// `AtomicUsize` so they update through `&self` alongside the map.
    hits: std::sync::atomic::AtomicUsize,
    misses: std::sync::atomic::AtomicUsize,
}

impl ClassViewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached `ClassView` for `fqcn`, building it from `file_path`
    /// on first request.
    ///
    /// Concurrency: `DashMap::entry` holds the FQCN's shard lock for the
    /// duration of the build. That's intentional — it dedupes two workers
    /// racing to build the *same* FQCN (the second waits and reuses the first's
    /// result), which is precisely the redundant analysis we're removing.
    /// `analyze` never re-enters this cache, so holding the shard lock across it
    /// can't deadlock; other FQCNs live on other shards and proceed in parallel.
    pub fn get_or_build(
        &self,
        fqcn: &str,
        file_path: &Path,
        project_root: &Path,
    ) -> Option<Arc<ClassView>> {
        use std::sync::atomic::Ordering::Relaxed;
        match self.cache.entry(fqcn.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                self.hits.fetch_add(1, Relaxed);
                e.get().clone()
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                self.misses.fetch_add(1, Relaxed);
                let view = analyze(file_path, project_root).map(Arc::new);
                slot.insert(view.clone());
                view
            }
        }
    }

    /// Number of lookups served from the cache without a rebuild. Test/log only.
    pub fn hits(&self) -> usize {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of first-time builds (distinct FQCNs analyzed). Test/log only.
    pub fn misses(&self) -> usize {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Resolve + classify every property-form member access captured in one file
/// into ingestible [`MagicMemberEntry`]s for the reverse reference index (M4).
///
/// Parses `source` once, then for each captured `member_access_ref` locates the
/// receiver node by its byte range and runs [`resolve_and_classify`]. Only
/// sites that resolve at HIGH or MEDIUM confidence are kept — the find-
/// references threshold — which also prunes the M2 capture firehose down to
/// real, classifiable usages. Unresolvable receivers and unknown members are
/// silently dropped.
///
/// `classviews` is reused across files by the caller so each model is analyzed
/// once per build pass.
///
/// `deps`, when provided, accumulates every receiver FQCN resolution
/// *attempted* — including accesses whose member classification fails — for
/// the magic dependency index (see `magic_dependency_index`). Recording
/// attempts rather than successes is what lets a later "member added to
/// class" save re-resolve the files that were waiting on it.
pub fn resolve_member_access_entries(
    source: &str,
    member_refs: &[Arc<MemberAccessReferenceData>],
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Vec<MagicMemberEntry> {
    if member_refs.is_empty() {
        return Vec::new();
    }
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let aliases = extract_use_aliases(&tree, source);
    let root = tree.root_node();

    let mut out = Vec::new();
    for m in member_refs {
        let Some(receiver) =
            root.descendant_for_byte_range(m.receiver_byte_start, m.receiver_byte_end)
        else {
            continue;
        };
        let Some(resolved) = resolve_and_classify(
            receiver,
            &m.member,
            m.form,
            bytes,
            &aliases,
            resolver,
            classviews,
            project_root,
            deps.as_deref_mut(),
        ) else {
            continue;
        };
        // find-references gate: HIGH + MEDIUM (rename will gate to HIGH later).
        if !matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
            continue;
        }
        // Call-form plain methods are every `->get()` / `->save()` in the
        // codebase — Intelephense's territory and pure index bloat. Only the
        // magic kinds (scope / finder / relationship) index from calls.
        // Property-form plain members stay (bounded: declared properties on
        // resolved classes). Facade methods (`Auth::check()`) are likewise
        // unbounded across a codebase; they're a goto/hover surface, not a
        // find-references one, so they don't index here either. Factory
        // resolutions (`Model::factory()` + chained factory methods) follow
        // the facade precedent: goto/hover only, no reverse-index entries.
        if m.form.is_call()
            && matches!(
                resolved.kind,
                MagicMemberKind::PlainMember
                    | MagicMemberKind::FacadeMethod
                    | MagicMemberKind::Factory
                    | MagicMemberKind::FactoryMethod
            )
        {
            continue;
        }
        out.push(MagicMemberEntry {
            fqcn: resolved.declaring_fqcn,
            member: m.member.clone(),
            line: m.line,
            column: m.column,
            end_column: m.end_column,
        });
    }
    out
}

/// Resolve a property-form receiver to its class, then classify `member`
/// against that class's resolved surfaces.
///
/// The pipeline: `receiver → FQCN + confidence` (via [`flow`] / `$this`) →
/// `FQCN → file_path` (via the class-hierarchy index) → `ClassView` (cached) →
/// [`classify_member`]. Returns `None` whenever any step can't proceed — an
/// unresolvable receiver, a class not in the index, or a member that matches
/// nothing — which is what prunes the M2 capture firehose down to real sites.
///
/// This is the M3 engine; M4 calls it during the reverse-index build and
/// writes the result into each site's reserved scaffold.
///
/// `deps`, when provided, records the receiver FQCN(s) this access resolves
/// to — *before* member classification, so failed lookups still register a
/// dependency on the receiver's class.
#[allow(clippy::too_many_arguments)]
pub fn resolve_and_classify(
    receiver: Node,
    member: &str,
    form: AccessForm,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Option<ResolvedMemberAccess> {
    // A container-resolution receiver records its *abstract* binding key as an
    // attempt dependency BEFORE any resolution — an `app('key')` site whose key
    // has no registration yet resolves to nothing and would otherwise record no
    // dependency at all, so the provider save that later ADDS the binding could
    // never ripple to it (#255). Mirrored branch-for-branch in
    // [`resolve_recipe_and_classify`].
    if let Some(d) = deps.as_deref_mut() {
        if let Some(key) = container_attempt_key(receiver, bytes) {
            d.insert(format!(
                "{}{key}",
                crate::magic_dependency_index::BINDING_DEP_PREFIX
            ));
        }
    }

    // A facade-alias receiver records its `alias:<token>` attempt dependency —
    // resolved-or-not, the exact analogue of the container branch above — so an
    // alias RETARGET ripples the OLD target's sites on the first save of a
    // session, when the empty registration baseline makes the diff see only the
    // new target added (#267). Mirrored in [`resolve_recipe_and_classify`].
    if let Some(d) = deps.as_deref_mut() {
        if let Some(key) = facade_alias_attempt_key(receiver, bytes, aliases) {
            d.insert(key);
        }
    }

    // Facade interception, checked first for a static-call name receiver —
    // `Auth::check()` — so the "resolved via facade" signal threads cleanly into
    // classification. A `Some` here means the concrete came through the facade
    // proxy (facade FQCN → accessor → bound concrete), so the member is a FACADE
    // method (tag `FacadeMethod`, goto the concrete's decl site) rather than a
    // plain method Intelephense owns. `resolve_receiver` runs the same
    // interception internally for any other caller; doing it explicitly here is
    // what carries the boolean to `classify_against`.
    let via_facade = matches!(receiver.kind(), "name" | "qualified_name");
    let facade_concrete = if via_facade {
        receiver.utf8_text(bytes).ok().and_then(|raw| {
            resolve_facade_receiver(receiver, raw, bytes, aliases, resolver, project_root)
        })
    } else {
        None
    };

    // A helper-chain receiver (`view()->make()`, `cache()->get()`) is the facade
    // case "one indirection over": the resolved concrete is a service whose
    // surface is largely forwarded via interfaces / `__call`, so its members are
    // tagged `FacadeMethod` too — that routes the goto/hover through
    // `facade_method_decl`'s contract-chase rather than dropping to Intelephense.
    // Checked only when facade interception didn't already fire.
    let helper_concrete = if facade_concrete.is_none() {
        resolve_helper_receiver(receiver, bytes, resolver)
    } else {
        None
    };

    let (fqcn, confidence, via_facade, via_factory) = match facade_concrete.or(helper_concrete) {
        Some((fqcn, confidence)) => (fqcn, confidence, true, false),
        None => {
            match resolve_receiver(receiver, bytes, aliases, resolver, classviews, project_root) {
                Some((fqcn, confidence)) => (fqcn, confidence, false, false),
                // Call-form receivers are frequently builder CHAINS
                // (`User::query()->active()`, `User::where(…)->active()`) whose
                // links the direct resolver can't type. The chain's subject is its
                // root — resolve that instead (#77 review). Property-form chains
                // (`User::first()->full_name`) deliberately stay chain-blind: the
                // column surface makes property terminals a far bigger
                // false-positive net than the call surfaces gated below.
                None if form.is_call() => {
                    let (fqcn, confidence, via_factory) = resolve_call_chain_receiver(
                        receiver,
                        bytes,
                        aliases,
                        resolver,
                        classviews,
                        project_root,
                    )?;
                    (fqcn, confidence, false, via_factory)
                }
                None => return None,
            }
        }
    };

    if let Some(d) = deps.as_deref_mut() {
        d.insert(fqcn.clone());
    }

    if let Some(resolved) = classify_against(
        &fqcn,
        member,
        form,
        confidence,
        via_facade,
        via_factory,
        resolver,
        classviews,
        project_root,
    ) {
        record_macro_decl_dep(&resolved, &fqcn, member, resolver, deps.as_deref_mut());
        return Some(resolved);
    }

    // Builder-typed receiver retry: `$query->active()` inside a scope body
    // types as the Eloquent Builder, which declares no scopes — retry against
    // the lexically enclosing class (the model whose scope body this is), the
    // same enclosing-model convention column rename uses. Gated to receivers
    // rooted in the enclosing `scope*` method's own parameter: a Builder
    // param inside a `whereHas` CLOSURE belongs to the related model, and
    // retrying it against the enclosing class would misattribute same-named
    // scopes. (Trade-off: correct attributions inside global-scope closures
    // are dropped too.) MEDIUM confidence — informational (every consumer
    // gate accepts High|Medium); the scope-param gate and classification are
    // the actual safety here.
    if form.is_call() && is_eloquent_builder(&fqcn) && is_scope_param_receiver(receiver, bytes) {
        let model = enclosing_class_fqcn(receiver, bytes)?;
        if let Some(d) = deps.as_deref_mut() {
            d.insert(model.clone());
        }
        let resolved = classify_against(
            &model,
            member,
            form,
            Confidence::Medium,
            false, // a builder retry never comes through a facade
            false, // …nor through a factory chain
            resolver,
            classviews,
            project_root,
        )?;
        record_macro_decl_dep(&resolved, &model, member, resolver, deps);
        return Some(resolved);
    }
    None
}

/// Record a macro's declaration file as a reverse-index dependency for a
/// site that classified as [`MagicMemberKind::Macro`] (#255). For an inline
/// `::macro()` registration that file IS the registering service provider,
/// so a provider-body edit's save ripple (`registration_ripple_keys`, keyed
/// on the provider path among others) reaches the site directly; the host
/// FQCN recorded by the resolution above covers the added/removed-key
/// directions. No-op for every other kind, or when the registry has no
/// entry (the classification just proved it does).
fn record_macro_decl_dep(
    resolved: &ResolvedMemberAccess,
    fqcn: &str,
    member: &str,
    resolver: &impl ClassFileResolver,
    deps: Option<&mut HashSet<String>>,
) {
    if resolved.kind != MagicMemberKind::Macro {
        return;
    }
    if let (Some(d), Some((decl_file, _))) = (deps, resolver.macro_target(fqcn, member)) {
        d.insert(decl_file.to_string_lossy().into_owned());
    }
}

/// Classify `member` against `fqcn`'s resolved surfaces — the shared tail of
/// [`resolve_and_classify`]'s direct path and its builder retry.
///
/// `via_facade` is the "receiver resolved through the facade proxy" signal: when
/// set, a member that would otherwise classify as a plain method is tagged
/// [`MagicMemberKind::FacadeMethod`] instead — a facade call's target IS the
/// concrete's decl site (goto-able / hoverable), NOT Intelephense's territory
/// the way a plain `$obj->method()` is. (Magic kinds — scope / accessor /
/// relationship / finder — still win over the facade tag: those are the
/// concrete's own Eloquent surfaces and resolve more precisely.)
///
/// `via_factory` is the chain analogue for a factory-rooted subject
/// (`User::factory()->…`, issue #30 item 3): `fqcn` is the resolved factory
/// class, and a member the factory hierarchy declares (a custom state, the
/// vendor `state`/`create`) re-tags [`MagicMemberKind::FactoryMethod`] so the
/// goto/hover consumers own it instead of dropping it as a plain method.
/// Unlike the facade signal it never degrades an undeclared member to the
/// class line — factories forward the remainder to the model/builder, and a
/// wrong-class target is worse than none.
#[allow(clippy::too_many_arguments)]
fn classify_against(
    fqcn: &str,
    member: &str,
    form: AccessForm,
    confidence: Confidence,
    via_facade: bool,
    via_factory: bool,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<ResolvedMemberAccess> {
    // A class the index knows: classify against its real surfaces first.
    if let Some(file_path) = resolver.class_file(fqcn) {
        if let Some(view) = classviews.get_or_build(fqcn, &file_path, project_root) {
            // `Model::factory()` (issue #30 item 3): the method is vendor
            // `HasFactory` magic, so classifying it against the model's own
            // surfaces would at best yield a droppable PlainMember. Resolve
            // the model → factory FQCN instead (`newFactory()` override or
            // convention); no resolvable factory falls through to the normal
            // surfaces. Checked first: a real `factory()` declaration wins
            // over `__callStatic` magic in PHP, and Eloquent's own dispatch
            // still lands on the trait method, never a `scopeFactory`.
            if form.is_call() && member == "factory" && view.kind == LaravelClassKind::Model {
                if let Some(factory) =
                    crate::factory_resolver::factory_fqcn_for_model(&view, resolver)
                {
                    return Some(ResolvedMemberAccess {
                        declaring_fqcn: factory,
                        kind: MagicMemberKind::Factory,
                        confidence,
                    });
                }
            }
            if let Some(classified) = classify_member(&view, member, form) {
                // A factory-chain member the factory hierarchy declares
                // (`->suspended()`, `->state()`) — a real target Intelephense
                // can't type without ide-helper. Re-tag so consumers keep it.
                if via_factory && classified.kind == MagicMemberKind::PlainMember {
                    return Some(ResolvedMemberAccess {
                        declaring_fqcn: classified.declaring_fqcn,
                        kind: MagicMemberKind::FactoryMethod,
                        confidence,
                    });
                }
                // A facade call whose member is just a plain method on the
                // concrete (`Auth::check()` → `AuthManager::check()`) is a real
                // goto target — tag it `FacadeMethod` so the consumers don't drop
                // it as Intelephense's. A magic kind keeps its own classification
                // (it's already a precise, goto-able surface).
                if via_facade && classified.kind == MagicMemberKind::PlainMember {
                    return Some(ResolvedMemberAccess {
                        declaring_fqcn: classified.declaring_fqcn,
                        kind: MagicMemberKind::FacadeMethod,
                        confidence,
                    });
                }
                return Some(ResolvedMemberAccess {
                    declaring_fqcn: classified.declaring_fqcn,
                    kind: classified.kind,
                    confidence,
                });
            }
        }
        // A facade call whose member is NOT declared on the concrete — the
        // `__call`/`@method` forwarding case (`Auth::guard()`,
        // `DB::beginTransaction()` are forwarded, not declared). The exact method
        // can't be located (chasing `__call`/guard chains is out of scope), so
        // DEGRADE to the concrete CLASS as the target — still useful — rather
        // than dropping to None. The declaring FQCN is the concrete itself; the
        // consumer falls back to the class's start line.
        if via_facade && form.is_call() {
            return Some(ResolvedMemberAccess {
                declaring_fqcn: fqcn.to_string(),
                kind: MagicMemberKind::FacadeMethod,
                confidence,
            });
        }
    }
    // Macro / mixin fallback: a call-form member that matches none of the class's
    // own surfaces may be a runtime-registered macro (`Str::macro('foo', …)`) or
    // a mixin method. Consulted last — after the real surfaces, before giving up
    // — and crucially OUTSIDE the `class_file` guard above: the dominant
    // Macroable hosts (`Str`, `Arr`, `Request`) are vendor classes the project
    // index doesn't carry, so requiring a buildable ClassView would drop every
    // framework-host macro. The registry is keyed on the resolved receiver FQCN
    // (exactly `fqcn`); the declaring class is that Macroable host. Only
    // call-form: a macro is reached via `__callStatic` / `__call`, never a
    // property read. The registry carries the true definition site for goto/hover.
    if form.is_call() && resolver.macro_target(fqcn, member).is_some() {
        return Some(ResolvedMemberAccess {
            declaring_fqcn: fqcn.to_string(),
            kind: MagicMemberKind::Macro,
            confidence,
        });
    }
    None
}

/// Resolve a call-chain receiver by its ROOT (#77 review). The chain's
/// subject stays its root only for genuine BUILDER chains, so the static
/// branch is gated twice:
///
/// - **First-link gate**: the root's static method must actually forward to
///   the query builder — an Eloquent chain starter (`query`, `where`,
///   `find`, …; the same list receiver detection uses) or one of the class's
///   own scopes / dynamic finders. Declared statics returning non-builder
///   objects (`factory()`, `fake()`, bespoke constructors) must NOT resolve:
///   Factory states routinely share names with scopes, and a scope rename
///   would rewrite `User::factory()->active()`.
/// - **Relation-hop bail**: a relationship link re-targets the chain's
///   subject to the related model (`$user->posts()->active()` is Post's
///   scope, not User's) — conservatively drop rather than misattribute.
///
/// Static roots resolve at HIGH (explicit class name; `static::` late-binds,
/// so it gets MEDIUM via the enclosing class as a lower bound). A variable
/// root re-enters the direct resolver capped at MEDIUM — informational
/// (consumer gates accept High|Medium alike); the gates above plus
/// classification are the real safety.
///
/// A `Model::factory()` root (issue #30 item 3) is the one deliberate
/// exception to the first-link gate: instead of refusing, the chain's subject
/// RE-TARGETS to the model's resolved factory class, and the returned
/// `via_factory` flag tells [`classify_against`] to tag its declared members
/// [`MagicMemberKind::FactoryMethod`]. Scope classification on the *model* is
/// still refused for factory chains — that's what the gate protects.
fn resolve_call_chain_receiver(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence, bool)> {
    let root = chain_root(receiver);
    if root.kind() == "scoped_call_expression" {
        let scope = root.child_by_field_name("scope")?;
        let (fqcn, confidence) = match scope.kind() {
            "name" | "qualified_name" => {
                let raw = scope.utf8_text(bytes).ok()?;
                (
                    qualify_fqcn(resolve_class_name(raw, aliases), scope, bytes),
                    Confidence::High,
                )
            }
            // `self::query()->…` / `static::query()->…` — the enclosing
            // class. `self` binds statically; `static` late-binds to the
            // runtime subclass, so the enclosing class is a lower bound.
            // `parent::` would need the parent FQCN from the hierarchy —
            // drops conservatively.
            "relative_scope" => {
                let raw = scope.utf8_text(bytes).ok()?;
                let fqcn = enclosing_class_fqcn(receiver, bytes)?;
                match raw {
                    "self" => (fqcn, Confidence::High),
                    "static" => (fqcn, Confidence::Medium),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let file = resolver.class_file(&fqcn)?;
        let view = classviews.get_or_build(&fqcn, &file, project_root)?;
        let first = root
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())?;
        // `Model::factory()->…` — the chain's subject is the FACTORY, not the
        // model. Re-target before the forwarding gate; the model-relationship
        // bail below doesn't apply (the links are factory members). A model
        // with no resolvable factory keeps the original refusal.
        if first == "factory" && view.kind == LaravelClassKind::Model {
            let factory = crate::factory_resolver::factory_fqcn_for_model(&view, resolver)?;
            return Some((factory, confidence, true));
        }
        let first_is_forwarding = crate::query_chain::methods::is_eloquent_static_starter(first)
            || matches!(
                classify_call(&view, first).map(|c| c.kind),
                Some(MagicMemberKind::Scope) | Some(MagicMemberKind::DynamicFinder)
            );
        if !first_is_forwarding {
            return None;
        }
        if has_relationship_link(receiver, bytes, &view) {
            return None;
        }
        return Some((fqcn, confidence, false));
    }
    if root.id() != receiver.id() {
        let (fqcn, confidence) =
            resolve_receiver(root, bytes, aliases, resolver, classviews, project_root)?;
        // Relation-hop bail, same reasoning as the static branch. A view
        // that can't build (e.g. the vendor Builder outside the fixture
        // graph) skips the check — classification downstream still gates.
        if let Some(file) = resolver.class_file(&fqcn) {
            if let Some(view) = classviews.get_or_build(&fqcn, &file, project_root) {
                if has_relationship_link(receiver, bytes, &view) {
                    return None;
                }
            }
        }
        let capped = match confidence {
            Confidence::High => Confidence::Medium,
            other => other,
        };
        return Some((fqcn, capped, false));
    }
    None
}

/// Does any link between the cursor's member and the chain root (exclusive)
/// name a relationship on `view`? For `$user->posts()->active()` with the
/// cursor on `active`, the receiver is the `posts()` call → links =
/// `["posts"]` → true when `posts` is a relationship.
fn has_relationship_link(
    receiver: Node,
    bytes: &[u8],
    view: &crate::laravel_introspector::chain::ClassView,
) -> bool {
    let mut cur = receiver;
    while matches!(
        cur.kind(),
        "member_call_expression"
            | "nullsafe_member_call_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
    ) {
        if let Some(name) = cur.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(bytes) {
                if view.relationships.iter().any(|r| r.method_name == text) {
                    return true;
                }
            }
        }
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    false
}

/// Does this receiver chain root back to a parameter of the enclosing
/// `scope*` method? Gates the builder→enclosing-model retry to canonical
/// scope bodies (`scopeRecent(Builder $query) { $query->active() }`). A
/// Builder param belonging to an intervening closure (`whereHas('posts',
/// fn (Builder $q) => $q->published())`) is the related model's builder, not
/// the enclosing model's — those must not retry. (A closure param that
/// SHADOWS the scope's param name still slips through; accepted edge.)
fn is_scope_param_receiver(receiver: Node, bytes: &[u8]) -> bool {
    let root = chain_root(receiver);
    if root.kind() != "variable_name" {
        return false;
    }
    let Some(var) = root
        .utf8_text(bytes)
        .ok()
        .map(|t| t.trim_start_matches('$'))
    else {
        return false;
    };
    let mut cur = root.parent();
    while let Some(n) = cur {
        if n.kind() == "method_declaration" {
            let is_scope = n
                .child_by_field_name("name")
                .and_then(|x| x.utf8_text(bytes).ok())
                .is_some_and(|name| name.len() > "scope".len() && name.starts_with("scope"));
            return is_scope && method_has_param(n, bytes, var);
        }
        cur = n.parent();
    }
    false
}

/// Is `$var` one of `method`'s declared parameters?
fn method_has_param(method: Node, bytes: &[u8], var: &str) -> bool {
    let Some(params) = method.child_by_field_name("parameters") else {
        return false;
    };
    let mut stack = vec![params];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_name" {
            if let Ok(text) = n.utf8_text(bytes) {
                if text.trim_start_matches('$') == var {
                    return true;
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// The root expression of a call/access chain: descend through the `object`
/// field of member calls/accesses. `User::query()->where(…)->active()` → the
/// `User::query()` scoped call; `$q->where(…)->active()` → `$q`.
fn chain_root(receiver: Node) -> Node {
    let mut cur = receiver;
    while matches!(
        cur.kind(),
        "member_call_expression"
            | "nullsafe_member_call_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
    ) {
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    cur
}

/// Is `fqcn` the Eloquent query builder (the type of a scope's `$query`
/// param)? The base `Query\Builder` is deliberately excluded — scopes don't
/// exist on it, and `DB::table(…)` chains must not retry against an
/// enclosing model they have nothing to do with.
fn is_eloquent_builder(fqcn: &str) -> bool {
    matches!(
        fqcn,
        "Illuminate\\Database\\Eloquent\\Builder"
            | "Illuminate\\Contracts\\Database\\Eloquent\\Builder"
    )
}

/// Resolve an arbitrary expression node to its class `(FQCN, confidence)` —
/// the public entry the view-variable inference uses to type a controller's
/// `view('x', ['user' => $expr])` values (and Volt `state`/`with`/`computed`).
///
/// First tries the flow chain classifier for inline Eloquent-producing
/// expressions (`User::all()`, `User::query()->first()`, `new User`) — the
/// dominant render-data shape. Falls back to the member-access receiver
/// resolution (bare variable via flow, `$this`, typed props, method returns,
/// auth helpers, …) for everything else.
pub fn resolve_expression_type(
    expr: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    flow::resolve_expression(expr, bytes, aliases)
        .or_else(|| resolve_receiver(expr, bytes, aliases, resolver, classviews, project_root))
}

/// Resolve a receiver expression node to `(FQCN, confidence)`.
///
/// Handles bare variables (`$user`, via flow tracking, with a `foreach`
/// fallback), `$this` (the enclosing class), typed properties (`$this->prop`),
/// and method-call results via the method's `self`/`static` return type
/// (`$user->fresh()->…`). The index + ClassView cache are threaded through for
/// the return-type case, which has to read the called method's declared type.
fn resolve_receiver(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    // Auth-helper receivers (`auth()->user()`, `Auth::user()`, `request()->
    // user()`) resolve to the configured auth user model — checked first
    // because they're specific call shapes the generic branches below would
    // otherwise mis-handle.
    if let Some(resolved) = resolve_auth_user_receiver(receiver, bytes, project_root) {
        return Some(resolved);
    }

    // Container-resolution receivers (`app('key')`, `resolve('key')`) resolve
    // the binding key to its registered concrete class. Checked before the
    // generic match because these are `function_call_expression`s the arms
    // below don't model, and the binding lookup rides in on `resolver`.
    if let Some(resolved) = resolve_container_receiver(receiver, bytes, resolver) {
        return Some(resolved);
    }

    // Helper-chain receivers (`view()->make()`, `cache()->get()`, …) resolve the
    // zero-arg helper's container service to its concrete class — the facade
    // resolution "one indirection over" (a global function rather than a static
    // proxy). Checked here, alongside the container case, before the generic
    // match the helper-call shape isn't modeled by.
    if let Some(resolved) = resolve_helper_receiver(receiver, bytes, resolver) {
        return Some(resolved);
    }

    match receiver.kind() {
        "variable_name" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let var = raw.trim_start_matches('$');
            if var == "this" {
                // `$this` is the enclosing class — a certain resolution.
                enclosing_class_fqcn(receiver, bytes).map(|fqcn| (fqcn, Confidence::High))
            } else {
                // Flow tracking first (assignments / typed params / `@var`);
                // then a `foreach` element type; then a Gate-ability closure's
                // first param (the authenticatable, untyped by convention).
                flow::resolve_with_confidence(receiver, bytes, var, aliases)
                    .or_else(|| resolve_foreach_var(receiver, bytes, var, aliases))
                    .or_else(|| resolve_gate_closure_user(receiver, bytes, project_root))
            }
        }
        // `$this->prop` — a typed property on the enclosing class.
        "member_access_expression" | "nullsafe_member_access_expression" => {
            resolve_typed_property(receiver, bytes, aliases)
        }
        // `$obj->method()` — resolve via the method's return type.
        "member_call_expression" | "nullsafe_member_call_expression" => {
            resolve_method_return(receiver, bytes, aliases, resolver, classviews, project_root)
        }
        // `User::…` — an explicit class-name receiver (static call). Resolve
        // through the file's use-aliases, then qualify a bare same-namespace
        // name with the file's namespace (PHP name-resolution semantics — a
        // sibling model needs no import). A name the class graph still doesn't
        // know (an unimported alias, a facade proxying elsewhere) stays
        // unresolved rather than guessed.
        "name" | "qualified_name" => {
            let raw = receiver.utf8_text(bytes).ok()?;

            // Facade interception, checked first: `Auth::check()`,
            // `\Auth::check()`, `\Illuminate\Support\Facades\Auth::check()`. A
            // facade is a thin static proxy — its own class only carries
            // `@method` docblocks, not the real members — so when the receiver
            // resolves to a facade we walk to the concrete implementation the
            // calls forward to (facade FQCN → accessor key → bound concrete) and
            // return THAT as the receiver type, so the member classifies against
            // the real class (`Auth::check` → `AuthManager`). This runs before
            // the plain class-index lookup below: the facade class itself may be
            // indexed (vendor), and resolving to it would classify against the
            // empty proxy instead of the implementation.
            if let Some(concrete) =
                resolve_facade_receiver(receiver, raw, bytes, aliases, resolver, project_root)
            {
                return Some(concrete);
            }

            let fqcn = qualify_fqcn(resolve_class_name(raw, aliases), receiver, bytes);
            if resolver.class_file(&fqcn).is_some() || resolver.has_macro_host(&fqcn) {
                // The class is indexed, OR it's a known Macroable host the index
                // doesn't carry (vendor `Str`/`Arr`/…) but the macro registry
                // does — either way the receiver resolves at HIGH (explicit
                // class name). Per-member classification still gates the result.
                Some((fqcn, Confidence::High))
            } else {
                None
            }
        }
        // `self::…` / `static::…` — the enclosing class. `self` binds
        // statically (HIGH); `static` late-binds to the runtime subclass, so
        // the enclosing class is a lower bound (MEDIUM). `parent::` would
        // need the parent's FQCN from the hierarchy — drops conservatively.
        // The keyword kinds are matched alongside `relative_scope` because a
        // byte-range receiver lookup lands on the anonymous keyword TOKEN
        // inside the `relative_scope` node, not the node itself.
        "relative_scope" | "self" | "static" | "parent" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let fqcn = enclosing_class_fqcn(receiver, bytes)?;
            match raw {
                "self" => Some((fqcn, Confidence::High)),
                "static" => Some((fqcn, Confidence::Medium)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve a static-call receiver THROUGH a facade to its concrete
/// implementation: `Auth::check()`, `\Auth::check()`,
/// `\Illuminate\Support\Facades\Auth::check()` → `Illuminate\Auth\AuthManager`.
///
/// A facade is a thin static proxy whose own class carries only `@method`
/// docblocks, never the real members — so when the receiver token resolves to a
/// facade we walk facade FQCN → accessor key → bound concrete and return THAT,
/// so the member classifies against the real class. Returns `None` when the
/// token isn't a facade (or its accessor has no bound concrete), letting the
/// plain class-index lookup take over.
///
/// Extracted from [`resolve_receiver`]'s `name`/`qualified_name` arm so
/// [`resolve_and_classify`] can call it directly: a `Some` here is the
/// "resolved via the facade interception" signal that classification keys on to
/// tag [`MagicMemberKind::FacadeMethod`] instead of a plain method (a facade
/// call's target is the concrete's decl site, NOT Intelephense's territory).
pub(crate) fn resolve_facade_receiver(
    receiver: Node,
    raw: &str,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let is_namespaced = file_namespace(receiver, bytes).is_some();
    let facade_fqcn = crate::facade_resolver::resolve_facade_fqcn(
        raw,
        aliases,
        &resolver.facade_aliases(),
        is_namespaced,
    )?;
    let concrete = crate::facade_resolver::facade_accessor(&facade_fqcn, project_root)
        .and_then(|accessor| resolver.binding_concrete(&accessor))?;
    Some((concrete, Confidence::High))
}

/// Resolve a container-resolution receiver — `app('key')` / `resolve('key')` —
/// to the concrete class its binding key was registered with.
///
/// The binding registry (reached through [`ClassFileResolver::binding_concrete`])
/// maps the key to the concrete class the developer bound
/// (`$this->app->singleton('currentTenant', Tenant::class)`), so a hit resolves
/// at HIGH confidence — the registration is explicit, not inferred. A key with
/// no registered binding, or one bound to a closure, returns a concrete the
/// class index won't know, so it drops to `None` downstream rather than guessing.
///
/// Only the string-keyed helper forms are handled here; the `::class` argument
/// form, `app()->make(…)`, and `App::make(…)` are separate receiver shapes.
fn resolve_container_receiver(
    receiver: Node,
    bytes: &[u8],
    resolver: &impl ClassFileResolver,
) -> Option<(String, Confidence)> {
    if receiver.kind() != "function_call_expression" {
        return None;
    }
    let func = receiver
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?
        .trim_start_matches('\\');
    if !matches!(func, "app" | "resolve") {
        return None;
    }
    let key = single_string_argument(receiver, bytes)?;
    let concrete = resolver.binding_concrete(&key)?;
    Some((concrete, Confidence::High))
}

/// Laravel global helpers that return a container-resolved service when called
/// with no positional argument, mapped to the container binding key that service
/// is registered under — the receiver-resolution analogue of a facade's
/// `getFacadeAccessor()`. Each entry is `(helper_name, binding_key)`.
///
/// Every key here is the *string* key the framework's service providers register
/// (the same keys the facades in [`crate::facade_resolver`] proxy), so it
/// resolves through the parsed binding registry via
/// [`ClassFileResolver::binding_concrete`] exactly like `app('key')`. The two
/// exceptions are noted inline:
///
/// - `response` is bound under a *contract* (`Illuminate\Contracts\Routing\
///   ResponseFactory`), never a string literal — the registry only stores
///   string-literal keys, so `binding_concrete` misses it. [`resolve_helper_receiver`]
///   detects the contract-shaped key and falls back to the interface→implementors
///   scan, which lands on the concrete `Illuminate\Routing\ResponseFactory`.
/// - `validator` is bound under the string `'validator'` (concrete
///   `Illuminate\Validation\Factory`), so it rides the registry like the rest.
///
/// Covered helpers: `view`, `cache`, `session`, `response`, `redirect`,
/// `cookie`, `config`, `validator`, `auth`.
const HELPER_BINDINGS: &[(&str, &str)] = &[
    ("view", "view"),
    ("cache", "cache"),
    ("session", "session"),
    // The `response()` helper resolves `ResponseFactory::class` from the
    // container; it has no string binding key, so this contract FQCN routes
    // through the implementors scan rather than the registry.
    (
        "response",
        "Illuminate\\Contracts\\Routing\\ResponseFactory",
    ),
    ("redirect", "redirect"),
    ("cookie", "cookie"),
    ("config", "config"),
    ("validator", "validator"),
    // `auth()->check()` / `auth()->guard()` etc. — the non-`user()` chains.
    // `resolve_auth_user_receiver` keeps its special-cased `->user()` exit
    // (it maps to the configured user MODEL, not the auth manager); every other
    // member on `auth()` classifies against the `auth` binding's concrete.
    ("auth", "auth"),
];

/// The binding key a zero-arg Laravel helper resolves its service under, if the
/// helper is one we model. `None` for an unmapped helper name.
fn helper_binding_key(helper: &str) -> Option<&'static str> {
    HELPER_BINDINGS
        .iter()
        .find(|(name, _)| *name == helper)
        .map(|(_, key)| *key)
}

/// Resolve a zero-argument Laravel helper receiver — `view()`, `cache()`,
/// `session()`, … — to the concrete class its container service resolves to,
/// the receiver-resolution analogue of [`resolve_facade_receiver`] "one
/// indirection over": where a facade proxies a binding through a static class,
/// the helper proxies the *same* binding through a global function call.
///
/// The helper's name maps to a container binding key ([`HELPER_BINDINGS`]); the
/// key resolves to its concrete FQCN through the parsed binding registry
/// ([`ClassFileResolver::binding_concrete`]) — a HIGH-confidence, explicit
/// registration — exactly as `app('key')` does. A contract-shaped key (only
/// `response`'s today) the string-keyed registry can't hold falls back to the
/// interface→implementors scan via [`resolve_interface_concrete`].
///
/// Only the zero-arg call form is handled: `view('welcome')` (the one-arg form)
/// renders a view rather than returning the factory, so a member access on it
/// (`view('welcome')->method()`) is the same factory receiver and resolves the
/// same way — the argument count is not gated here, only that the function is a
/// known helper. An unmapped helper, or a mapped helper whose binding has no
/// concrete, returns `None` so the caller falls through cleanly.
fn resolve_helper_receiver(
    receiver: Node,
    bytes: &[u8],
    resolver: &impl ClassFileResolver,
) -> Option<(String, Confidence)> {
    if receiver.kind() != "function_call_expression" {
        return None;
    }
    let func = receiver
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?
        .trim_start_matches('\\');
    let key = helper_binding_key(func)?;
    // A string-literal binding key resolves through the registry; a
    // contract-shaped key (carries a `\` — `response`'s `ResponseFactory`) the
    // registry never holds resolves through the implementors scan instead.
    if key.contains('\\') {
        resolve_interface_concrete(key, resolver).map(|c| (c, Confidence::High))
    } else {
        resolver
            .binding_concrete(key)
            .map(|c| (c, Confidence::High))
    }
}

/// Resolve an interface/contract FQCN to the single concrete class that
/// implements it, for the contract→concrete fallback shared by the helper-chain
/// (`response()`) and method-return (`view()->make()->render()`) paths.
///
/// Disambiguation rule:
/// - **single implementer** → that concrete (HIGH-confidence resolution; the
///   choice is unambiguous).
/// - **multiple implementers** → pick the highest package-priority one if that
///   is unambiguous; otherwise return `None`. We never fabricate a target by
///   guessing among equals.
/// - **no implementer** → `None`.
///
/// Package priority follows the project-wide convention (App=2 > Package=1 >
/// Framework=0): a project's own implementation of a framework contract wins
/// over the vendor default. The priority is read from the resolved file path
/// (`vendor/` segment ⇒ Framework/Package, otherwise App) since the
/// [`ClassFileResolver`] seam carries file paths, not parsed package metadata.
fn resolve_interface_concrete(
    interface_fqcn: &str,
    resolver: &impl ClassFileResolver,
) -> Option<String> {
    let impls = resolver.implementers_of(interface_fqcn);
    match impls.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        many => {
            // Multiple implementers: prefer a single highest-priority one. An
            // app-level (non-`vendor/`) implementation outranks a vendor one;
            // ties (more than one at the top priority) are ambiguous → `None`.
            let mut ranked: Vec<(u8, &String)> = many
                .iter()
                .map(|fqcn| (impl_priority(fqcn, resolver), fqcn))
                .collect();
            ranked.sort_by_key(|r| std::cmp::Reverse(r.0));
            let top = ranked[0].0;
            let top_count = ranked.iter().filter(|(p, _)| *p == top).count();
            if top_count == 1 {
                Some(ranked[0].1.clone())
            } else {
                None
            }
        }
    }
}

/// Package priority of an implementing class for disambiguation: App=2 (the
/// project's own code) outranks vendor code=0. Derived from the resolved file
/// path — a `vendor/` segment marks framework/package code. An unresolvable
/// file (not in the index) is treated as lowest priority.
fn impl_priority(fqcn: &str, resolver: &impl ClassFileResolver) -> u8 {
    match resolver.class_file(fqcn) {
        Some(path) => {
            if path.components().any(|c| c.as_os_str() == "vendor") {
                0
            } else {
                2
            }
        }
        None => 0,
    }
}

/// The sole string-literal argument of a call — `'currentTenant'` in
/// `app('currentTenant')`. Returns `None` when the call has zero or more than
/// one argument, or the lone argument isn't a plain string literal (a variable,
/// a `::class` const, a concatenation — none of which name a binding key here).
fn single_string_argument(call: Node, bytes: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut named = args.named_children(&mut cursor);
    let first = named.next()?;
    if named.next().is_some() {
        return None;
    }
    // tree-sitter-php wraps each actual argument in an `argument` node.
    let expr = if first.kind() == "argument" {
        first.named_child(0)?
    } else {
        first
    };
    string_literal_value(expr, bytes)
}

/// The content of a single/double-quoted string literal node, or `None`.
fn string_literal_value(node: Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    Some(
        node.utf8_text(bytes)
            .ok()?
            .trim_matches(['\'', '"'])
            .to_string(),
    )
}

/// Resolve a `$user`-style receiver that is the first parameter of a Gate
/// ability closure (`Gate::define('x', function ($user) { … })`,
/// `Gate::before`/`after`, or the `gate()` helper form) to the auth user model.
///
/// Laravel contractually passes the authenticatable as the first argument to
/// these closures, so an *untyped* first param resolves with HIGH confidence.
/// This is the common `HorizonServiceProvider::gate()` shape that flow tracking
/// can't reach (no type hint, no assignment).
fn resolve_gate_closure_user(
    var_node: Node,
    bytes: &[u8],
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let var = var_node.utf8_text(bytes).ok()?.trim_start_matches('$');
    let closure = enclosing_closure(var_node)?;
    if !is_first_param(closure, bytes, var) {
        return None;
    }
    if !is_gate_ability_closure(closure, bytes) {
        return None;
    }
    auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
}

/// Nearest enclosing closure (`function () {}` / `fn () =>`) of `node`.
fn enclosing_closure(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "anonymous_function" | "anonymous_function_creation_expression" | "arrow_function"
        ) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Whether `var` names the first formal parameter of `closure`.
fn is_first_param(closure: Node, bytes: &[u8], var: &str) -> bool {
    let Some(params) = closure.child_by_field_name("parameters") else {
        return false;
    };
    let mut c = params.walk();
    let Some(first) = params
        .named_children(&mut c)
        .find(|p| p.kind() == "simple_parameter")
    else {
        return false;
    };
    first
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        .map(|t| t.trim_start_matches('$') == var)
        .unwrap_or(false)
}

/// Whether `closure` is an argument to `Gate::define` / `before` / `after`
/// (facade) or the `gate()->define(...)` helper — the ability-definition calls
/// whose first closure param is the authenticatable.
fn is_gate_ability_closure(closure: Node, bytes: &[u8]) -> bool {
    // Step out through any argument / arguments wrappers to the enclosing call.
    let mut node = closure;
    let call = loop {
        let Some(p) = node.parent() else {
            return false;
        };
        if matches!(p.kind(), "argument" | "arguments") {
            node = p;
            continue;
        }
        break p;
    };
    let name = call
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok());
    if !matches!(name, Some("define" | "before" | "after")) {
        return false;
    }
    match call.kind() {
        "scoped_call_expression" => call
            .child_by_field_name("scope")
            .and_then(|s| s.utf8_text(bytes).ok())
            .map(|s| s.rsplit('\\').next().unwrap_or(s) == "Gate")
            .unwrap_or(false),
        "member_call_expression" | "nullsafe_member_call_expression" => call
            .child_by_field_name("object")
            .map(|o| {
                o.kind() == "function_call_expression"
                    && o.child_by_field_name("function")
                        .and_then(|f| f.utf8_text(bytes).ok())
                        == Some("gate")
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Resolve an auth-helper receiver to the configured auth user model:
/// `auth()->user()`, `request()->user()` (member calls on the `auth()` /
/// `request()` helpers) and `Auth::user()` (the facade). These are the
/// dominant way authenticated-user attributes are reached in real code, and
/// the user model is well-known, so they resolve at HIGH confidence.
fn resolve_auth_user_receiver(
    receiver: Node,
    bytes: &[u8],
    project_root: &Path,
) -> Option<(String, Confidence)> {
    match receiver.kind() {
        // `Auth::user()` / `\Illuminate\Support\Facades\Auth::user()`
        "scoped_call_expression" => {
            let name = receiver
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?;
            if name != "user" {
                return None;
            }
            let scope = receiver
                .child_by_field_name("scope")?
                .utf8_text(bytes)
                .ok()?;
            let base = scope.rsplit('\\').next().unwrap_or(scope);
            if base != "Auth" {
                return None;
            }
            auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
        }
        // `auth()->user()` / `request()->user()`
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let name = receiver
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?;
            if name != "user" {
                return None;
            }
            let object = receiver.child_by_field_name("object")?;
            if object.kind() != "function_call_expression" {
                return None;
            }
            let func = object
                .child_by_field_name("function")?
                .utf8_text(bytes)
                .ok()?;
            if func == "auth" || func == "request" {
                auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve a `$obj->method()` receiver via the called method's return type —
/// the second hop of a chain like `view()->make()->render()`.
///
/// Handled cases (return-type inference is indirect → [`Confidence::Medium`]):
/// - **`self` / `static`** — the canonical fluent / return-`$this` shape
///   (`$user->activated()->email`) resolves to the object's own class.
/// - **A concrete class return type** — surfaced so a second-hop receiver
///   classifies against it (`Illuminate\View\Factory::make()` declares
///   `: View`, so `make()->render()` types as `Illuminate\View\View`). The
///   declared name is normalized against the *declaring* file's namespace and
///   `use` aliases (the type is written in that file's namespace, not the
///   caller's), then accepted only if the class graph knows it.
/// - **An interface / contract return type** — falls back to the concrete
///   implementor via [`resolve_interface_concrete`] (the binding registry only
///   stores string-literal keys, never `Contract::class`, so a contract return
///   resolves through the implementors scan, never the registry). A contract
///   with a single implementer resolves; ambiguous multi-implementer contracts
///   return `None` rather than fabricating a target.
///
/// Vendor methods whose return types live outside the indexed graph (or that
/// the ClassView walk can't see — it stops at `Eloquent\Model`) drop to `None`.
fn resolve_method_return(
    call: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let object = call.child_by_field_name("object")?;
    let name_node = call.child_by_field_name("name")?;
    if name_node.kind() != "name" {
        return None;
    }
    let method = name_node.utf8_text(bytes).ok()?;
    let (obj_fqcn, _) =
        resolve_receiver(object, bytes, aliases, resolver, classviews, project_root)?;
    let file_path = resolver.class_file(&obj_fqcn)?;
    let view = classviews.get_or_build(&obj_fqcn, &file_path, project_root)?;
    let ret = method_return_type(&view, method)?;
    let normalized = normalize_type(&ret)?;
    if matches!(normalized.as_str(), "self" | "static") {
        return Some((obj_fqcn, Confidence::Medium));
    }
    // A named return type: re-qualify it in the DECLARING file's namespace —
    // the type is written there, not in the caller's context — then surface the
    // concrete it denotes.
    let candidate = qualify_return_type(&normalized, &view);
    // Interface/contract first: an indexed interface with implementor(s) resolves
    // to its concrete implementor (the binding registry can't hold a
    // `Contract::class` key, so this scan is the only path). A concrete class the
    // graph knows is surfaced directly.
    if let Some(concrete) = resolve_interface_concrete(&candidate, resolver) {
        return Some((concrete, Confidence::Medium));
    }
    if resolver.class_file(&candidate).is_some() {
        return Some((candidate, Confidence::Medium));
    }
    None
}

/// Re-qualify a method's declared return-type name in the namespace of the
/// class that declares it. The return type is written in the *declaring* file's
/// namespace with that file's `use` imports, so a bare `View` in
/// `Illuminate\View\Factory` means `Illuminate\View\View` (its import), not the
/// caller's `View`. Reads the declaring file once (cached transitively by the
/// caller's `ClassView` build) to recover its aliases + namespace.
fn qualify_return_type(name: &str, view: &ClassView) -> String {
    let aliases = std::fs::read_to_string(&view.file_path)
        .ok()
        .map(|content| crate::laravel_introspector::model_metadata::extract_use_aliases(&content))
        .unwrap_or_default();
    crate::laravel_introspector::model_metadata::resolve_to_fqcn(
        name,
        view.namespace.as_deref(),
        &aliases,
    )
    .trim_start_matches('\\')
    .to_string()
}

/// The declared return type of `method` on `view` (raw form preferred so
/// `self`/`static` survive), searching the inheritance-resolved method set.
fn method_return_type(view: &ClassView, method: &str) -> Option<String> {
    view.all_methods
        .iter()
        .find(|m| m.value.name == method)
        .and_then(|m| {
            m.value
                .return_type_raw
                .clone()
                .or_else(|| m.value.return_type.clone())
        })
}

/// Resolve a `$this->prop` receiver via the declared type of `prop` on the
/// enclosing class — both ordinary typed properties (`private User $prop;`) and
/// constructor-promoted ones (`public function __construct(private User $prop)`).
///
/// An explicitly declared type is as certain as a typed parameter, so this is
/// [`Confidence::High`]. Only `$this->prop` is handled (the runtime class of
/// `$other->prop` would itself need resolving first); union / intersection
/// types are skipped as ambiguous.
fn resolve_typed_property(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    let object = receiver.child_by_field_name("object")?;
    if object.kind() != "variable_name" || object.utf8_text(bytes).ok()? != "$this" {
        return None;
    }
    let name_node = receiver.child_by_field_name("name")?;
    if name_node.kind() != "name" {
        return None;
    }
    let prop = name_node.utf8_text(bytes).ok()?;
    let class = enclosing_class_node(receiver)?;
    let raw_type = property_type_in_class(class, bytes, prop)?;
    let normalized = normalize_type(&raw_type)?;
    let resolved = resolve_class_name(&normalized, aliases);
    Some((qualify_fqcn(resolved, receiver, bytes), Confidence::High))
}

/// Turn a resolved type name into a fully-qualified one. `resolve_class_name`
/// expands `use`-aliases and absolute (`\Foo`) names, but leaves a bare
/// same-namespace name unqualified — so qualify those with the file's
/// namespace (matching how the class-hierarchy index keys its FQCNs).
///
/// TODO: a namespace-RELATIVE qualified name (`Models\User` inside
/// `namespace App;`) is treated as already-qualified and won't resolve to
/// `App\Models\User` — a false negative both the static-receiver arm and the
/// chain-root arm inherit.
///
/// `pub` so the provider-binding closure resolver
/// ([`crate::salsa_impl::resolve_closure_concrete`]) reuses the SAME
/// namespace-qualification the static-receiver arm uses: a binding bound to a
/// bare same-namespace `new X` (e.g. `singleton('auth', fn ($app) => new
/// AuthManager($app))` in a provider that lives in `AuthManager`'s namespace)
/// must qualify `AuthManager` → `Illuminate\Auth\AuthManager` before the
/// on-disk gate, or the binding degrades to `"Closure"`.
pub fn qualify_fqcn(name: String, node: Node, bytes: &[u8]) -> String {
    let trimmed = name.trim_start_matches('\\').to_string();
    if trimmed.contains('\\') {
        // Already namespaced (alias-resolved or absolute).
        trimmed
    } else if let Some(ns) = file_namespace(node, bytes) {
        format!("{ns}\\{trimmed}")
    } else {
        trimmed
    }
}

/// Find the declared type of property `$prop` on `class` — scanning both
/// `property_declaration`s and constructor-promoted parameters. Does not
/// descend into nested (anonymous) classes.
fn property_type_in_class(class: Node, bytes: &[u8], prop: &str) -> Option<String> {
    let mut stack = vec![class];
    while let Some(n) = stack.pop() {
        // Don't leak into a nested class's members.
        if n.id() != class.id() && matches!(n.kind(), "class_declaration" | "anonymous_class") {
            continue;
        }

        if n.kind() == "property_declaration" {
            if let Some(ty) = n.child_by_field_name("type") {
                let mut c = n.walk();
                let matches_name = n.children(&mut c).any(|child| {
                    child.kind() == "property_element"
                        && child
                            .child_by_field_name("name")
                            .and_then(|nm| nm.utf8_text(bytes).ok())
                            .map(|t| t.trim_start_matches('$') == prop)
                            .unwrap_or(false)
                });
                if matches_name {
                    return ty.utf8_text(bytes).ok().map(str::to_string);
                }
            }
        }

        if n.kind() == "property_promotion_parameter" {
            let is_match = n
                .child_by_field_name("name")
                .and_then(|nm| nm.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$') == prop)
                .unwrap_or(false);
            if is_match {
                if let Some(ty) = n.child_by_field_name("type") {
                    return ty.utf8_text(bytes).ok().map(str::to_string);
                }
            }
        }

        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Normalize a declared type to a single resolvable class name: strip a
/// leading `?` (nullable), reject union / intersection types as ambiguous.
fn normalize_type(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches('?').trim();
    if t.is_empty() || t.contains('|') || t.contains('&') {
        return None;
    }
    Some(t.to_string())
}

/// Resolve a `foreach ($coll as $var)` value variable to its element type.
///
/// The element type is the model the collection operates on — `flow::resolve`
/// already gives that for a collection variable (`$users = User::all()` →
/// `User`), and the element of that collection is a `User`. Inferring an
/// element from a collection is indirect, so this is [`Confidence::Medium`].
/// (A `@var User $var` on the loop is found by flow directly, before this
/// fallback runs.)
fn resolve_foreach_var(
    use_site: Node,
    bytes: &[u8],
    var: &str,
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    let mut cur = use_site.parent();
    while let Some(n) = cur {
        if n.kind() == "foreach_statement" {
            if let Some((collection, value_var)) = foreach_parts(n, bytes) {
                if value_var == var {
                    // Only a collection *variable* is resolvable here; flow
                    // tracks its model type.
                    if collection.kind() == "variable_name" {
                        let cvar = collection.utf8_text(bytes).ok()?.trim_start_matches('$');
                        if let Some(fqcn) = flow::resolve(collection, bytes, cvar, aliases) {
                            return Some((fqcn, Confidence::Medium));
                        }
                    }
                    // Matched the binding but couldn't resolve the collection.
                    return None;
                }
            }
        }
        cur = n.parent();
    }
    None
}

/// Extract `(collection_expr, value_var_name)` from a `foreach_statement`.
/// Handles `foreach ($c as $v)` and `foreach ($c as $k => $v)`; list
/// destructuring (`as [$a, $b]`) is not handled.
fn foreach_parts<'t>(foreach: Node<'t>, bytes: &[u8]) -> Option<(Node<'t>, String)> {
    let body_id = foreach.child_by_field_name("body").map(|b| b.id());
    let mut named = Vec::new();
    let mut c = foreach.walk();
    for ch in foreach.named_children(&mut c) {
        if Some(ch.id()) == body_id {
            continue;
        }
        named.push(ch);
    }
    if named.len() < 2 {
        return None;
    }
    let collection = named[0];
    let binding = named[named.len() - 1];
    let value_var = match binding.kind() {
        "variable_name" => binding
            .utf8_text(bytes)
            .ok()?
            .trim_start_matches('$')
            .to_string(),
        "pair" => {
            // `$key => $value` — the value is the pair's last named child.
            let mut pc = binding.walk();
            let kids: Vec<_> = binding.named_children(&mut pc).collect();
            let last = kids.last()?;
            if last.kind() != "variable_name" {
                return None;
            }
            last.utf8_text(bytes)
                .ok()?
                .trim_start_matches('$')
                .to_string()
        }
        _ => return None,
    };
    Some((collection, value_var))
}

/// FQCN of the class lexically enclosing `node`, or `None` when `node` isn't
/// inside a class (e.g. a free function, or a `$this` inside a trait — whose
/// runtime class is unknowable statically).
pub(crate) fn enclosing_class_fqcn(node: Node, bytes: &[u8]) -> Option<String> {
    let class = enclosing_class_node(node)?;
    let class_name = class
        .child_by_field_name("name")
        .and_then(|x| x.utf8_text(bytes).ok())?;
    match file_namespace(node, bytes) {
        Some(ns) => Some(format!("{ns}\\{class_name}")),
        None => Some(class_name.to_string()),
    }
}

/// The class-like node lexically enclosing `node`, if any — a named
/// `class_declaration` or an `anonymous_class` (Volt SFC `new class extends
/// Component`). Matching anonymous classes lets `$this->prop` typed-property
/// resolution work inside Volt components (the class has no FQCN, but its
/// property declarations carry types just the same).
fn enclosing_class_node(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(n.kind(), "class_declaration" | "anonymous_class") {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The file's `namespace ...;` declaration, if any. Walks to the tree root and
/// finds the first `namespace_definition`.
fn file_namespace(node: Node, bytes: &[u8]) -> Option<String> {
    let mut root = node;
    while let Some(p) = root.parent() {
        root = p;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "namespace_definition" {
            if let Some(nn) = n.child_by_field_name("name") {
                return nn.utf8_text(bytes).ok().map(str::to_string);
            }
            // Fallback: the first `namespace_name` child (field name varies
            // across grammar versions).
            let mut c = n.walk();
            let name_node = n.children(&mut c).find(|ch| ch.kind() == "namespace_name");
            if let Some(nn) = name_node {
                return nn.utf8_text(bytes).ok().map(str::to_string);
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// The configured auth user model FQCN for `project_root`, parsed from
/// `config/auth.php`'s `providers.users.model`. Memoized per project root for
/// the process lifetime — the auth model effectively never changes during a
/// session, so we don't re-read the file on every receiver resolution.
///
/// (If `config/auth.php` is edited mid-session the cached value goes stale
/// until restart — an acceptable tradeoff for a value this stable.)
fn auth_model_fqcn(project_root: &Path) -> Option<String> {
    static MEMO: once_cell::sync::Lazy<std::sync::Mutex<HashMap<PathBuf, Option<String>>>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

    if let Ok(memo) = MEMO.lock() {
        if let Some(cached) = memo.get(project_root) {
            return cached.clone();
        }
    }
    let resolved = std::fs::read_to_string(project_root.join("config/auth.php"))
        .ok()
        .and_then(|content| parse_auth_model(&content));
    if let Ok(mut memo) = MEMO.lock() {
        memo.insert(project_root.to_path_buf(), resolved.clone());
    }
    resolved
}

/// Extract `providers.users.model` from `config/auth.php` source. Resolves the
/// class reference (`User::class`, `env('AUTH_MODEL', User::class)`, or a
/// fully-qualified `\App\Models\User::class`) through the file's `use` aliases.
/// Tree-sitter parsing means commented-out providers are ignored. Returns the
/// first `'model'` entry in source order — the default user provider.
fn parse_auth_model(content: &str) -> Option<String> {
    let tree = parse_php(content).ok()?;
    let bytes = content.as_bytes();
    let aliases = extract_use_aliases(&tree, content);

    let mut best: Option<(usize, String)> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "array_element_initializer" {
            let mut c = n.walk();
            let kids: Vec<_> = n.named_children(&mut c).collect();
            if kids.len() == 2 {
                let key = kids[0].utf8_text(bytes).ok()?.trim_matches(['\'', '"']);
                if key == "model" {
                    if let Some(class_ref) = first_class_const(kids[1], bytes) {
                        let fqcn = resolve_class_name(&class_ref, &aliases)
                            .trim_start_matches('\\')
                            .to_string();
                        let pos = n.start_byte();
                        if best.as_ref().is_none_or(|(p, _)| pos < *p) {
                            best = Some((pos, fqcn));
                        }
                    }
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    best.map(|(_, fqcn)| fqcn)
}

/// First `X::class` class reference in `node`'s subtree → the class name `X`
/// (handles both a bare `User::class` and one wrapped in `env(..., User::class)`).
fn first_class_const(node: Node, bytes: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "class_constant_access_expression" {
            let scope = n.named_child(0)?;
            return scope.utf8_text(bytes).ok().map(str::to_string);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Recognize Eloquent dynamic finders: `where{Column}` / `orWhere{Column}`
/// where `{Column}` is a StudlyCase column in the model's column surface.
///
/// Multi-segment finders (`whereEmailAndStatus`) are not handled — only the
/// single-column form, which covers the overwhelming majority of real usage.
fn classify_dynamic_finder(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    let column = dynamic_finder_column(member)?;
    if view.column_surface.iter().any(|c| c.name == column) {
        return Some(ClassifiedMember {
            declaring_fqcn: view.fqcn.clone(),
            kind: MagicMemberKind::DynamicFinder,
        });
    }
    None
}

/// The column a dynamic finder name targets: `whereEmail` / `orWhereEmail` →
/// `email`. `None` when the name isn't finder-shaped (`where`, `whereabouts`).
/// Shared by classification and the goto/hover dispatch (a finder has no
/// declaring method — its definition site is the column's migration line).
pub fn dynamic_finder_column(member: &str) -> Option<String> {
    let rest = member
        .strip_prefix("where")
        .or_else(|| member.strip_prefix("orWhere"))?;
    // Must have a StudlyCase remainder — guards against `where`/`whereabouts`.
    if !rest.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    Some(pascal_to_snake(rest))
}

// ═══════════════════════════════════════════════════════════════════════════
// M1 single-parse capture — compile (parse time) + eval (resolve time)
// ═══════════════════════════════════════════════════════════════════════════
//
// The functions below let the whole-project magic build stop re-reading and
// re-parsing each target file. `capture_*` runs at PARSE (tree in hand) and
// compiles each site's INTRA-file receiver resolution into a small owned
// `ReceiverRecipeData`; `eval_*` / `resolve_member_access_entries_with_context`
// run at RESOLVE and finish the CROSS-file half against snapshots + memos,
// mirroring `resolve_and_classify` / `resolve_receiver` branch-for-branch so
// the resolved entries + deps are byte-identical to the re-parse path. The
// tree functions above stay the live-query path AND the equivalence baseline.

use crate::salsa_impl::{
    ChainRecipeData, ChainRootData, MemberContextData, ReceiverRecipeData, SiteContextData,
    ValueExprPlanData,
};
use tree_sitter::Tree;

// ─── Compile: PHP member-access sites ──────────────────────────────────────

/// Compile every PHP member-access site into its captured [`SiteContextData`],
/// positionally parallel to `refs`. Receivers are located by the SAME byte
/// range the resolve pass uses, so node identity matches by construction.
pub(crate) fn capture_php_sites(
    source: &str,
    tree: &Tree,
    refs: &[Arc<MemberAccessReferenceData>],
    aliases: &UseAliases,
) -> Vec<SiteContextData> {
    let bytes = source.as_bytes();
    let root = tree.root_node();
    refs.iter()
        .map(|m| {
            let Some(receiver) =
                root.descendant_for_byte_range(m.receiver_byte_start, m.receiver_byte_end)
            else {
                return SiteContextData {
                    recipe: ReceiverRecipeData::Unresolvable,
                    chain: None,
                    enclosing_class_fqcn: None,
                    is_scope_param_receiver: false,
                };
            };
            let recipe = compile_receiver(receiver, bytes, aliases);
            // The builder-chain fallback + retry are call-form-only, so only
            // call-form sites pay to capture them.
            let (chain, enclosing, is_scope_param) = if m.form.is_call() {
                (
                    compile_chain(receiver, bytes, aliases),
                    enclosing_class_fqcn(receiver, bytes),
                    is_scope_param_receiver(receiver, bytes),
                )
            } else {
                (None, None, false)
            };
            SiteContextData {
                recipe,
                chain,
                enclosing_class_fqcn: enclosing,
                is_scope_param_receiver: is_scope_param,
            }
        })
        .collect()
}

/// Compile a Blade receiver expression (from its `<?php {receiver};` snippet)
/// into a recipe — the parse-time form of `resolve_chain_receiver`'s
/// `resolve_expression_type`. Bare `$var` / `$this->prop` receivers compile to
/// `Unresolvable` (they resolve from the view-var / Volt-prop maps by text at
/// eval, never through the recipe).
pub(crate) fn compile_blade_site(receiver_text: &str) -> SiteContextData {
    let recipe = compile_blade_receiver(receiver_text);
    SiteContextData {
        recipe,
        chain: None,
        enclosing_class_fqcn: None,
        is_scope_param_receiver: false,
    }
}

fn compile_blade_receiver(receiver_text: &str) -> ReceiverRecipeData {
    let snippet = format!("<?php {receiver_text};");
    let Ok(tree) = parse_php(&snippet) else {
        return ReceiverRecipeData::Unresolvable;
    };
    let bytes = snippet.as_bytes();
    let aliases = extract_use_aliases(&tree, &snippet);
    let Some(expr) = first_snippet_expression(&tree) else {
        return ReceiverRecipeData::Unresolvable;
    };
    // Mirror `resolve_expression_type`: flow classifier first, receiver second.
    match flow::resolve_expression(expr, bytes, &aliases) {
        Some((fqcn, confidence)) => ReceiverRecipeData::Resolved { fqcn, confidence },
        None => compile_receiver(expr, bytes, &aliases),
    }
}

/// The expression of the first `expression_statement` in a `<?php <expr>;`
/// snippet — the receiver node. (Mirrors `view_var_index::first_expression`.)
fn first_snippet_expression(tree: &Tree) -> Option<Node<'_>> {
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "expression_statement" {
            let mut c = n.walk();
            return n.named_children(&mut c).next();
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Compile a value expression (a `view()`-data or Volt value) — the parse-time
/// form of `resolve_expression_type`: flow classifier → `Resolved`, else the
/// receiver recipe.
pub(crate) fn compile_value_expr(
    expr: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> ValueExprPlanData {
    match flow::resolve_expression(expr, bytes, aliases) {
        Some((fqcn, confidence)) => ValueExprPlanData::Resolved { fqcn, confidence },
        None => ValueExprPlanData::Recipe(compile_receiver(expr, bytes, aliases)),
    }
}

/// Compile a `flow::resolve`-typed value (the `compact('x')` case, which types
/// purely intra-file) into a plan — always `Resolved` or omitted by the caller.
pub(crate) fn compile_flow_var(
    node: Node,
    bytes: &[u8],
    var: &str,
    aliases: &UseAliases,
) -> Option<ValueExprPlanData> {
    flow::resolve_with_confidence(node, bytes, var, aliases)
        .map(|(fqcn, confidence)| ValueExprPlanData::Resolved { fqcn, confidence })
}

/// Compile a receiver node into a recipe — the intra-file half of
/// [`resolve_receiver`]. `resolve_and_classify` checks auth BEFORE the shape
/// match, and an auth miss falls through to the rest of `resolve_receiver`, so
/// the auth recipe carries that fallthrough as its `fallback`.
fn compile_receiver(receiver: Node, bytes: &[u8], aliases: &UseAliases) -> ReceiverRecipeData {
    if auth_user_receiver_shape(receiver, bytes) {
        return ReceiverRecipeData::AuthUser {
            fallback: Box::new(compile_receiver_after_auth(receiver, bytes, aliases)),
        };
    }
    compile_receiver_after_auth(receiver, bytes, aliases)
}

/// The rest of [`resolve_receiver`] after the auth check: container → helper →
/// the node-kind match.
fn compile_receiver_after_auth(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> ReceiverRecipeData {
    if let Some(key) = container_receiver_key(receiver, bytes) {
        return ReceiverRecipeData::ContainerKey(key);
    }
    if let Some(name) = helper_receiver_name(receiver, bytes) {
        return ReceiverRecipeData::HelperBinding(name);
    }
    match receiver.kind() {
        "variable_name" => {
            let Ok(raw) = receiver.utf8_text(bytes) else {
                return ReceiverRecipeData::Unresolvable;
            };
            let var = raw.trim_start_matches('$');
            if var == "this" {
                match enclosing_class_fqcn(receiver, bytes) {
                    Some(fqcn) => ReceiverRecipeData::Resolved {
                        fqcn,
                        confidence: Confidence::High,
                    },
                    None => ReceiverRecipeData::Unresolvable,
                }
            } else if let Some((fqcn, confidence)) =
                flow::resolve_with_confidence(receiver, bytes, var, aliases)
                    .or_else(|| resolve_foreach_var(receiver, bytes, var, aliases))
            {
                ReceiverRecipeData::Resolved { fqcn, confidence }
            } else if is_gate_closure_user_shape(receiver, bytes) {
                ReceiverRecipeData::GateClosureUser
            } else {
                ReceiverRecipeData::Unresolvable
            }
        }
        "member_access_expression" | "nullsafe_member_access_expression" => {
            match resolve_typed_property(receiver, bytes, aliases) {
                Some((fqcn, confidence)) => ReceiverRecipeData::Resolved { fqcn, confidence },
                None => ReceiverRecipeData::Unresolvable,
            }
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            // `resolve_method_return` requires a plain `name` method node.
            let Some(name_node) = receiver.child_by_field_name("name") else {
                return ReceiverRecipeData::Unresolvable;
            };
            if name_node.kind() != "name" {
                return ReceiverRecipeData::Unresolvable;
            }
            let (Ok(method), Some(object)) = (
                name_node.utf8_text(bytes),
                receiver.child_by_field_name("object"),
            ) else {
                return ReceiverRecipeData::Unresolvable;
            };
            ReceiverRecipeData::MethodReturn {
                object: Box::new(compile_receiver(object, bytes, aliases)),
                method: method.to_string(),
            }
        }
        "name" | "qualified_name" => {
            let Ok(raw) = receiver.utf8_text(bytes) else {
                return ReceiverRecipeData::Unresolvable;
            };
            ReceiverRecipeData::StaticName {
                raw: raw.to_string(),
                qualified: qualify_fqcn(resolve_class_name(raw, aliases), receiver, bytes),
                is_namespaced: file_namespace(receiver, bytes).is_some(),
            }
        }
        "relative_scope" | "self" | "static" | "parent" => {
            let Ok(raw) = receiver.utf8_text(bytes) else {
                return ReceiverRecipeData::Unresolvable;
            };
            match enclosing_class_fqcn(receiver, bytes) {
                Some(fqcn) => match raw {
                    "self" => ReceiverRecipeData::Resolved {
                        fqcn,
                        confidence: Confidence::High,
                    },
                    "static" => ReceiverRecipeData::Resolved {
                        fqcn,
                        confidence: Confidence::Medium,
                    },
                    _ => ReceiverRecipeData::Unresolvable,
                },
                None => ReceiverRecipeData::Unresolvable,
            }
        }
        _ => ReceiverRecipeData::Unresolvable,
    }
}

/// Compile the builder-chain fallback — the parse-time form of
/// [`resolve_call_chain_receiver`]. `None` when the receiver roots in itself
/// (no chain to walk).
fn compile_chain(receiver: Node, bytes: &[u8], aliases: &UseAliases) -> Option<ChainRecipeData> {
    let root = chain_root(receiver);
    let links = chain_link_names(receiver, bytes);
    if root.kind() == "scoped_call_expression" {
        let scope = root.child_by_field_name("scope")?;
        let (qualified, confidence) = match scope.kind() {
            "name" | "qualified_name" => {
                let raw = scope.utf8_text(bytes).ok()?;
                (
                    qualify_fqcn(resolve_class_name(raw, aliases), scope, bytes),
                    Confidence::High,
                )
            }
            "relative_scope" => {
                let raw = scope.utf8_text(bytes).ok()?;
                let fqcn = enclosing_class_fqcn(receiver, bytes)?;
                match raw {
                    "self" => (fqcn, Confidence::High),
                    "static" => (fqcn, Confidence::Medium),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let first_method = root
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())?
            .to_string();
        return Some(ChainRecipeData {
            root: ChainRootData::StaticScope {
                qualified,
                confidence,
                first_method,
            },
            links,
        });
    }
    if root.id() != receiver.id() {
        return Some(ChainRecipeData {
            root: ChainRootData::Var(Box::new(compile_receiver(root, bytes, aliases))),
            links,
        });
    }
    None
}

/// The member names walked from `receiver` toward (but not including) the chain
/// root — the input to the relation-hop bail (see [`has_relationship_link`]).
fn chain_link_names(receiver: Node, bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = receiver;
    while matches!(
        cur.kind(),
        "member_call_expression"
            | "nullsafe_member_call_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
    ) {
        if let Some(name) = cur.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(bytes) {
                out.push(text.to_string());
            }
        }
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    out
}

// ─── Compile: intra-file SHAPE predicates ──────────────────────────────────
// The shape half of the cross-file `resolve_*_receiver` functions — "does this
// receiver look like X?" without doing the cross-file resolution. Kept in
// lockstep with the resolvers they mirror.

/// The shape of [`resolve_auth_user_receiver`] — `Auth::user()`,
/// `auth()->user()`, `request()->user()` — without the auth-model lookup.
fn auth_user_receiver_shape(receiver: Node, bytes: &[u8]) -> bool {
    match receiver.kind() {
        "scoped_call_expression" => {
            let Some(name) = receiver
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
            else {
                return false;
            };
            if name != "user" {
                return false;
            }
            receiver
                .child_by_field_name("scope")
                .and_then(|s| s.utf8_text(bytes).ok())
                .map(|scope| scope.rsplit('\\').next().unwrap_or(scope) == "Auth")
                .unwrap_or(false)
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let Some(name) = receiver
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
            else {
                return false;
            };
            if name != "user" {
                return false;
            }
            let Some(object) = receiver.child_by_field_name("object") else {
                return false;
            };
            object.kind() == "function_call_expression"
                && matches!(
                    object
                        .child_by_field_name("function")
                        .and_then(|f| f.utf8_text(bytes).ok()),
                    Some("auth") | Some("request")
                )
        }
        _ => false,
    }
}

/// The container-binding key of [`resolve_container_receiver`] — the `'key'` in
/// `app('key')` / `resolve('key')`.
fn container_receiver_key(receiver: Node, bytes: &[u8]) -> Option<String> {
    if receiver.kind() != "function_call_expression" {
        return None;
    }
    let func = receiver
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?
        .trim_start_matches('\\');
    if !matches!(func, "app" | "resolve") {
        return None;
    }
    single_string_argument(receiver, bytes)
}

/// The helper name of [`resolve_helper_receiver`] — a zero-arg helper whose
/// container binding we model (`view`, `cache`, …).
fn helper_receiver_name(receiver: Node, bytes: &[u8]) -> Option<String> {
    if receiver.kind() != "function_call_expression" {
        return None;
    }
    let func = receiver
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?
        .trim_start_matches('\\');
    helper_binding_key(func).map(|_| func.to_string())
}

/// The registry-shaped container-binding key a receiver *attempts* — the
/// `'key'` in `app('key')` / `resolve('key')` ([`container_receiver_key`]) or a
/// mapped zero-arg helper's binding key ([`helper_binding_key`]) — independent
/// of whether the key resolves. Contract-shaped helper keys (`response`'s
/// `Illuminate\Contracts\…` — they carry a `\`) are excluded: the string-keyed
/// binding registry never holds them, so a registration diff never emits them.
/// Facade-accessor indirection (`Auth::…` → accessor key) is deliberately NOT
/// covered: extracting the accessor requires parsing the facade class even on
/// failed resolutions, and a facade site that resolved records the concrete
/// FQCN the diff already emits.
fn container_attempt_key(receiver: Node, bytes: &[u8]) -> Option<String> {
    container_receiver_key(receiver, bytes).or_else(|| {
        if receiver.kind() != "function_call_expression" {
            return None;
        }
        let func = receiver
            .child_by_field_name("function")?
            .utf8_text(bytes)
            .ok()?
            .trim_start_matches('\\');
        helper_binding_key(func)
            .filter(|key| !key.contains('\\'))
            .map(str::to_string)
    })
}

/// The `alias:<token>` reverse-index attempt key for a facade receiver that
/// resolves through the global alias map (`Auth::check()`, `\Cache::get()`), or
/// `None` for any other receiver shape. The token comes from
/// [`crate::facade_resolver::global_alias_token`] — the exact gate
/// `resolve_facade_fqcn` applies — so the key is recorded against the same tokens
/// the registration diff emits, and resolved-or-not (independent of whether the
/// token is a registered alias today) so a retarget reaches this site on the
/// first empty-baseline save (#267). Mirrors [`container_attempt_key`] for the
/// facade-alias kind.
fn facade_alias_attempt_key(receiver: Node, bytes: &[u8], aliases: &UseAliases) -> Option<String> {
    if !matches!(receiver.kind(), "name" | "qualified_name") {
        return None;
    }
    let raw = receiver.utf8_text(bytes).ok()?;
    let is_namespaced = file_namespace(receiver, bytes).is_some();
    crate::facade_resolver::global_alias_token(raw, aliases, is_namespaced)
        .map(|token| crate::magic_dependency_index::alias_dep_key(&token))
}

/// The shape of [`resolve_gate_closure_user`] — a variable that is a Gate
/// ability closure's first parameter — without the auth-model lookup.
fn is_gate_closure_user_shape(var_node: Node, bytes: &[u8]) -> bool {
    let Ok(raw) = var_node.utf8_text(bytes) else {
        return false;
    };
    let var = raw.trim_start_matches('$');
    let Some(closure) = enclosing_closure(var_node) else {
        return false;
    };
    is_first_param(closure, bytes, var) && is_gate_ability_closure(closure, bytes)
}

// ─── Eval: resolve captured context against snapshots ──────────────────────

/// Pass-2 replacement: resolve captured PHP member sites without re-reading or
/// re-parsing the target file. Byte-identical to
/// [`resolve_member_access_entries`] for the same input.
pub fn resolve_member_access_entries_with_context(
    ctx: &MemberContextData,
    member_refs: &[Arc<MemberAccessReferenceData>],
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Vec<MagicMemberEntry> {
    debug_assert_eq!(
        ctx.sites.len(),
        member_refs.len(),
        "captured sites must be positionally parallel to member_access_refs"
    );
    let mut out = Vec::new();
    for (m, site) in member_refs.iter().zip(ctx.sites.iter()) {
        let Some(resolved) = resolve_recipe_and_classify(
            site,
            &ctx.aliases,
            &m.member,
            m.form,
            resolver,
            classviews,
            project_root,
            deps.as_deref_mut(),
        ) else {
            continue;
        };
        // find-references gate: HIGH + MEDIUM (mirrors the tree path).
        if !matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
            continue;
        }
        if m.form.is_call()
            && matches!(
                resolved.kind,
                MagicMemberKind::PlainMember
                    | MagicMemberKind::FacadeMethod
                    | MagicMemberKind::Factory
                    | MagicMemberKind::FactoryMethod
            )
        {
            continue;
        }
        out.push(MagicMemberEntry {
            fqcn: resolved.declaring_fqcn,
            member: m.member.clone(),
            line: m.line,
            column: m.column,
            end_column: m.end_column,
        });
    }
    out
}

/// The captured-context port of [`resolve_and_classify`], branch-for-branch.
#[allow(clippy::too_many_arguments)]
fn resolve_recipe_and_classify(
    site: &SiteContextData,
    aliases: &UseAliases,
    member: &str,
    form: AccessForm,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Option<ResolvedMemberAccess> {
    // Container-binding attempt dependency (see [`resolve_and_classify`]'s
    // mirror): the abstract key is recorded resolved-or-not, so a provider
    // save that ADDS the binding ripples to this site (#255).
    if let Some(d) = deps.as_deref_mut() {
        let key = match &site.recipe {
            ReceiverRecipeData::ContainerKey(key) => Some(key.as_str()),
            ReceiverRecipeData::HelperBinding(name) => {
                helper_binding_key(name).filter(|key| !key.contains('\\'))
            }
            _ => None,
        };
        if let Some(key) = key {
            d.insert(format!(
                "{}{key}",
                crate::magic_dependency_index::BINDING_DEP_PREFIX
            ));
        }
    }

    // Facade-alias attempt dependency (see [`resolve_and_classify`]'s mirror):
    // the `alias:<token>` key is recorded resolved-or-not so an alias retarget
    // ripples this site on the first (empty-baseline) save (#267).
    if let Some(d) = deps.as_deref_mut() {
        if let ReceiverRecipeData::StaticName {
            raw, is_namespaced, ..
        } = &site.recipe
        {
            if let Some(token) =
                crate::facade_resolver::global_alias_token(raw, aliases, *is_namespaced)
            {
                d.insert(crate::magic_dependency_index::alias_dep_key(&token));
            }
        }
    }

    // Outer facade interception — a static-name receiver only.
    let facade_concrete = match &site.recipe {
        ReceiverRecipeData::StaticName {
            raw, is_namespaced, ..
        } => eval_facade(raw, *is_namespaced, aliases, resolver, project_root),
        _ => None,
    };
    // Outer helper interception, only when facade didn't fire.
    let helper_concrete = if facade_concrete.is_none() {
        match &site.recipe {
            ReceiverRecipeData::HelperBinding(name) => eval_helper(name, resolver),
            _ => None,
        }
    } else {
        None
    };

    let (fqcn, confidence, via_facade, via_factory) = match facade_concrete.or(helper_concrete) {
        Some((fqcn, confidence)) => (fqcn, confidence, true, false),
        None => match eval_receiver(&site.recipe, aliases, resolver, classviews, project_root) {
            Some((fqcn, confidence)) => (fqcn, confidence, false, false),
            None if form.is_call() => {
                let chain = site.chain.as_ref()?;
                let (fqcn, confidence, via_factory) =
                    eval_chain(chain, aliases, resolver, classviews, project_root)?;
                (fqcn, confidence, false, via_factory)
            }
            None => return None,
        },
    };

    if let Some(d) = deps.as_deref_mut() {
        d.insert(fqcn.clone());
    }

    if let Some(resolved) = classify_against(
        &fqcn,
        member,
        form,
        confidence,
        via_facade,
        via_factory,
        resolver,
        classviews,
        project_root,
    ) {
        record_macro_decl_dep(&resolved, &fqcn, member, resolver, deps.as_deref_mut());
        return Some(resolved);
    }

    // Builder retry (see [`resolve_and_classify`]): captured `is_scope_param`
    // gate + captured enclosing class.
    if form.is_call() && is_eloquent_builder(&fqcn) && site.is_scope_param_receiver {
        let model = site.enclosing_class_fqcn.clone()?;
        if let Some(d) = deps.as_deref_mut() {
            d.insert(model.clone());
        }
        let resolved = classify_against(
            &model,
            member,
            form,
            Confidence::Medium,
            false,
            false,
            resolver,
            classviews,
            project_root,
        )?;
        record_macro_decl_dep(&resolved, &model, member, resolver, deps);
        return Some(resolved);
    }
    None
}

/// The captured-context port of [`resolve_receiver`].
pub(crate) fn eval_receiver(
    recipe: &ReceiverRecipeData,
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    match recipe {
        ReceiverRecipeData::Resolved { fqcn, confidence } => Some((fqcn.clone(), *confidence)),
        ReceiverRecipeData::AuthUser { fallback } => auth_model_fqcn(project_root)
            .map(|m| (m, Confidence::High))
            .or_else(|| eval_receiver(fallback, aliases, resolver, classviews, project_root)),
        ReceiverRecipeData::GateClosureUser => {
            auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
        }
        ReceiverRecipeData::ContainerKey(key) => resolver
            .binding_concrete(key)
            .map(|c| (c, Confidence::High)),
        ReceiverRecipeData::HelperBinding(name) => eval_helper(name, resolver),
        ReceiverRecipeData::StaticName {
            raw,
            qualified,
            is_namespaced,
        } => eval_facade(raw, *is_namespaced, aliases, resolver, project_root).or_else(|| {
            if resolver.class_file(qualified).is_some() || resolver.has_macro_host(qualified) {
                Some((qualified.clone(), Confidence::High))
            } else {
                None
            }
        }),
        ReceiverRecipeData::MethodReturn { object, method } => {
            eval_method_return(object, method, aliases, resolver, classviews, project_root)
        }
        ReceiverRecipeData::Unresolvable => None,
    }
}

/// Eval a [`ValueExprPlanData`] — the captured-context port of
/// [`resolve_expression_type`].
pub(crate) fn eval_value_expr(
    plan: &ValueExprPlanData,
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    match plan {
        ValueExprPlanData::Resolved { fqcn, confidence } => Some((fqcn.clone(), *confidence)),
        ValueExprPlanData::Recipe(recipe) => {
            eval_receiver(recipe, aliases, resolver, classviews, project_root)
        }
    }
}

/// The captured-context port of [`resolve_facade_receiver`].
fn eval_facade(
    raw: &str,
    is_namespaced: bool,
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let facade_fqcn = crate::facade_resolver::resolve_facade_fqcn(
        raw,
        aliases,
        &resolver.facade_aliases(),
        is_namespaced,
    )?;
    let concrete = crate::facade_resolver::facade_accessor(&facade_fqcn, project_root)
        .and_then(|accessor| resolver.binding_concrete(&accessor))?;
    Some((concrete, Confidence::High))
}

/// The captured-context port of [`resolve_helper_receiver`]'s tail.
fn eval_helper(name: &str, resolver: &impl ClassFileResolver) -> Option<(String, Confidence)> {
    let key = helper_binding_key(name)?;
    if key.contains('\\') {
        resolve_interface_concrete(key, resolver).map(|c| (c, Confidence::High))
    } else {
        resolver
            .binding_concrete(key)
            .map(|c| (c, Confidence::High))
    }
}

/// The captured-context port of [`resolve_call_chain_receiver`].
fn eval_chain(
    chain: &ChainRecipeData,
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence, bool)> {
    match &chain.root {
        ChainRootData::StaticScope {
            qualified,
            confidence,
            first_method,
        } => {
            let file = resolver.class_file(qualified)?;
            let view = classviews.get_or_build(qualified, &file, project_root)?;
            // `Model::factory()->…` re-targets to the factory (see
            // [`resolve_call_chain_receiver`]'s static branch).
            if first_method == "factory" && view.kind == LaravelClassKind::Model {
                let factory = crate::factory_resolver::factory_fqcn_for_model(&view, resolver)?;
                return Some((factory, *confidence, true));
            }
            let first_is_forwarding =
                crate::query_chain::methods::is_eloquent_static_starter(first_method)
                    || matches!(
                        classify_call(&view, first_method).map(|c| c.kind),
                        Some(MagicMemberKind::Scope) | Some(MagicMemberKind::DynamicFinder)
                    );
            if !first_is_forwarding {
                return None;
            }
            if chain_links_hit_relationship(&chain.links, &view) {
                return None;
            }
            Some((qualified.clone(), *confidence, false))
        }
        ChainRootData::Var(obj_recipe) => {
            let (fqcn, confidence) =
                eval_receiver(obj_recipe, aliases, resolver, classviews, project_root)?;
            if let Some(file) = resolver.class_file(&fqcn) {
                if let Some(view) = classviews.get_or_build(&fqcn, &file, project_root) {
                    if chain_links_hit_relationship(&chain.links, &view) {
                        return None;
                    }
                }
            }
            let capped = match confidence {
                Confidence::High => Confidence::Medium,
                other => other,
            };
            Some((fqcn, capped, false))
        }
    }
}

/// Does any captured chain link name a relationship on `view`? The
/// captured-context form of [`has_relationship_link`].
fn chain_links_hit_relationship(links: &[String], view: &ClassView) -> bool {
    links
        .iter()
        .any(|l| view.relationships.iter().any(|r| r.method_name == *l))
}

/// The captured-context port of [`resolve_method_return`].
fn eval_method_return(
    object: &ReceiverRecipeData,
    method: &str,
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let (obj_fqcn, _) = eval_receiver(object, aliases, resolver, classviews, project_root)?;
    let file_path = resolver.class_file(&obj_fqcn)?;
    let view = classviews.get_or_build(&obj_fqcn, &file_path, project_root)?;
    let ret = method_return_type(&view, method)?;
    let normalized = normalize_type(&ret)?;
    if matches!(normalized.as_str(), "self" | "static") {
        return Some((obj_fqcn, Confidence::Medium));
    }
    let candidate = qualify_return_type(&normalized, &view);
    if let Some(concrete) = resolve_interface_concrete(&candidate, resolver) {
        return Some((concrete, Confidence::Medium));
    }
    if resolver.class_file(&candidate).is_some() {
        return Some((candidate, Confidence::Medium));
    }
    None
}

#[cfg(test)]
mod tests;
