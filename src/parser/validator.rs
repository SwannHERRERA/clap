// Internal
use crate::builder::StyledStr;
use crate::builder::{Arg, ArgPredicate, Command, PossibleValue};
use crate::error::{Error, Result as ClapResult};
use crate::output::Usage;
use crate::parser::{ArgMatcher, ParseState};
use crate::util::ChildGraph;
use crate::util::FlatMap;
use crate::util::FlatSet;
use crate::util::Id;
use crate::INTERNAL_ERROR_MSG;

pub(crate) struct Validator<'cmd> {
    cmd: &'cmd Command,
    required: ChildGraph<Id>,
}

impl<'cmd> Validator<'cmd> {
    pub(crate) fn new(cmd: &'cmd Command) -> Self {
        let required = cmd.required_graph();
        Validator { cmd, required }
    }

    pub(crate) fn validate(
        &mut self,
        parse_state: ParseState,
        matcher: &mut ArgMatcher,
    ) -> ClapResult<()> {
        debug!("Validator::validate");
        let mut conflicts = Conflicts::new();
        let has_subcmd = matcher.subcommand_name().is_some();

        if let ParseState::Opt(a) = parse_state {
            debug!("Validator::validate: needs_val_of={:?}", a);

            let o = &self.cmd[&a];
            let should_err = if let Some(v) = matcher.args.get(o.get_id()) {
                v.all_val_groups_empty() && o.get_min_vals() != 0
            } else {
                true
            };
            if should_err {
                return Err(Error::empty_value(
                    self.cmd,
                    &get_possible_values_cli(o)
                        .iter()
                        .filter(|pv| !pv.is_hide_set())
                        .map(|n| n.get_name().to_owned())
                        .collect::<Vec<_>>(),
                    o.to_string(),
                ));
            }
        }

        if !has_subcmd && self.cmd.is_arg_required_else_help_set() {
            let num_user_values = matcher
                .arg_ids()
                .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
                .count();
            if num_user_values == 0 {
                let message = self.cmd.write_help_err(false);
                return Err(Error::display_help_error(self.cmd, message));
            }
        }
        if !has_subcmd && self.cmd.is_subcommand_required_set() {
            let bn = self
                .cmd
                .get_bin_name()
                .unwrap_or_else(|| self.cmd.get_name());
            return Err(Error::missing_subcommand(
                self.cmd,
                bn.to_string(),
                Usage::new(self.cmd)
                    .required(&self.required)
                    .create_usage_with_title(&[]),
            ));
        }

        ok!(self.validate_conflicts(matcher, &mut conflicts));
        if !(self.cmd.is_subcommand_negates_reqs_set() && has_subcmd) {
            ok!(self.validate_required(matcher, &mut conflicts));
        }

        Ok(())
    }

    fn validate_conflicts(
        &mut self,
        matcher: &ArgMatcher,
        conflicts: &mut Conflicts,
    ) -> ClapResult<()> {
        debug!("Validator::validate_conflicts");

        ok!(self.validate_exclusive(matcher));

        for arg_id in matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
            .filter(|arg_id| self.cmd.find(arg_id).is_some())
        {
            debug!("Validator::validate_conflicts::iter: id={:?}", arg_id);
            let conflicts = conflicts.gather_conflicts(self.cmd, matcher, arg_id);
            ok!(self.build_conflict_err(arg_id, &conflicts, matcher));
        }

        Ok(())
    }

    fn validate_exclusive(&self, matcher: &ArgMatcher) -> ClapResult<()> {
        debug!("Validator::validate_exclusive");
        let args_count = matcher
            .arg_ids()
            .filter(|arg_id| {
                matcher.check_explicit(arg_id, &crate::builder::ArgPredicate::IsPresent)
            })
            .count();
        if args_count <= 1 {
            // Nothing present to conflict with
            return Ok(());
        }

        matcher
            .arg_ids()
            .filter(|arg_id| {
                matcher.check_explicit(arg_id, &crate::builder::ArgPredicate::IsPresent)
            })
            .filter_map(|name| {
                debug!("Validator::validate_exclusive:iter:{:?}", name);
                self.cmd
                    .find(name)
                    // Find `arg`s which are exclusive but also appear with other args.
                    .filter(|&arg| arg.is_exclusive_set() && args_count > 1)
            })
            // Throw an error for the first conflict found.
            .try_for_each(|arg| {
                Err(Error::argument_conflict(
                    self.cmd,
                    arg.to_string(),
                    Vec::new(),
                    Usage::new(self.cmd)
                        .required(&self.required)
                        .create_usage_with_title(&[]),
                ))
            })
    }

    fn build_conflict_err(
        &self,
        name: &Id,
        conflict_ids: &[Id],
        matcher: &ArgMatcher,
    ) -> ClapResult<()> {
        if conflict_ids.is_empty() {
            return Ok(());
        }

        debug!("Validator::build_conflict_err: name={:?}", name);
        let mut seen = FlatSet::new();
        let conflicts = conflict_ids
            .iter()
            .flat_map(|c_id| {
                if self.cmd.find_group(c_id).is_some() {
                    self.cmd.unroll_args_in_group(c_id)
                } else {
                    vec![c_id.clone()]
                }
            })
            .filter_map(|c_id| {
                seen.insert(c_id.clone()).then(|| {
                    let c_arg = self.cmd.find(&c_id).expect(INTERNAL_ERROR_MSG);
                    c_arg.to_string()
                })
            })
            .collect();

        let former_arg = self.cmd.find(name).expect(INTERNAL_ERROR_MSG);
        let usg = self.build_conflict_err_usage(matcher, conflict_ids);
        Err(Error::argument_conflict(
            self.cmd,
            former_arg.to_string(),
            conflicts,
            usg,
        ))
    }

    fn build_conflict_err_usage(
        &self,
        matcher: &ArgMatcher,
        conflicting_keys: &[Id],
    ) -> Option<StyledStr> {
        let used_filtered: Vec<Id> = matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
            .filter(|n| {
                // Filter out the args we don't want to specify.
                self.cmd.find(n).map_or(false, |a| !a.is_hide_set())
            })
            .filter(|key| !conflicting_keys.contains(key))
            .cloned()
            .collect();
        let required: Vec<Id> = used_filtered
            .iter()
            .filter_map(|key| self.cmd.find(key))
            .flat_map(|arg| arg.requires.iter().map(|item| &item.1))
            .filter(|key| !used_filtered.contains(key) && !conflicting_keys.contains(key))
            .chain(used_filtered.iter())
            .cloned()
            .collect();
        Usage::new(self.cmd)
            .required(&self.required)
            .create_usage_with_title(&required)
    }

    fn gather_requires(&mut self, matcher: &ArgMatcher) {
        debug!("Validator::gather_requires");
        for name in matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
        {
            debug!("Validator::gather_requires:iter:{:?}", name);
            if let Some(arg) = self.cmd.find(name) {
                let is_relevant = |(val, req_arg): &(ArgPredicate, Id)| -> Option<Id> {
                    let required = matcher.check_explicit(arg.get_id(), val);
                    required.then(|| req_arg.clone())
                };

                for req in self.cmd.unroll_arg_requires(is_relevant, arg.get_id()) {
                    self.required.insert(req);
                }
            } else if let Some(g) = self.cmd.find_group(name) {
                debug!("Validator::gather_requires:iter:{:?}:group", name);
                for r in &g.requires {
                    self.required.insert(r.clone());
                }
            }
        }
    }

    fn validate_required(
        &mut self,
        matcher: &ArgMatcher,
        conflicts: &mut Conflicts,
    ) -> ClapResult<()> {
        debug!("Validator::validate_required: required={:?}", self.required);
        self.gather_requires(matcher);

        let mut missing_required = Vec::new();
        let mut highest_index = 0;

        let is_exclusive_present = matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
            .any(|id| {
                self.cmd
                    .find(id)
                    .map(|arg| arg.is_exclusive_set())
                    .unwrap_or_default()
            });
        debug!(
            "Validator::validate_required: is_exclusive_present={}",
            is_exclusive_present
        );

        for arg_or_group in self
            .required
            .iter()
            .filter(|r| !matcher.check_explicit(r, &ArgPredicate::IsPresent))
        {
            debug!("Validator::validate_required:iter:aog={:?}", arg_or_group);
            if let Some(arg) = self.cmd.find(arg_or_group) {
                debug!("Validator::validate_required:iter: This is an arg");
                if !is_exclusive_present && !self.is_missing_required_ok(arg, matcher, conflicts) {
                    debug!(
                        "Validator::validate_required:iter: Missing {:?}",
                        arg.get_id()
                    );
                    missing_required.push(arg.get_id().clone());
                    if !arg.is_last_set() {
                        highest_index = highest_index.max(arg.get_index().unwrap_or(0));
                    }
                }
            } else if let Some(group) = self.cmd.find_group(arg_or_group) {
                debug!("Validator::validate_required:iter: This is a group");
                if !self
                    .cmd
                    .unroll_args_in_group(&group.id)
                    .iter()
                    .any(|a| matcher.check_explicit(a, &ArgPredicate::IsPresent))
                {
                    debug!(
                        "Validator::validate_required:iter: Missing {:?}",
                        group.get_id()
                    );
                    missing_required.push(group.get_id().clone());
                }
            }
        }

        // Validate the conditionally required args
        for a in self
            .cmd
            .get_arguments()
            .filter(|a| !matcher.check_explicit(a.get_id(), &ArgPredicate::IsPresent))
        {
            let mut required = false;

            for (other, val) in &a.r_ifs {
                if matcher.check_explicit(other, &ArgPredicate::Equals(val.into())) {
                    debug!(
                        "Validator::validate_required:iter: Missing {:?}",
                        a.get_id()
                    );
                    required = true;
                }
            }

            let match_all = a.r_ifs_all.iter().all(|(other, val)| {
                matcher.check_explicit(other, &ArgPredicate::Equals(val.into()))
            });
            if match_all && !a.r_ifs_all.is_empty() {
                debug!(
                    "Validator::validate_required:iter: Missing {:?}",
                    a.get_id()
                );
                required = true;
            }

            if (!a.r_unless.is_empty() || !a.r_unless_all.is_empty())
                && self.fails_arg_required_unless(a, matcher)
            {
                debug!(
                    "Validator::validate_required:iter: Missing {:?}",
                    a.get_id()
                );
                required = true;
            }

            if required {
                missing_required.push(a.get_id().clone());
                if !a.is_last_set() {
                    highest_index = highest_index.max(a.get_index().unwrap_or(0));
                }
            }
        }

        // For display purposes, include all of the preceding positional arguments
        if !self.cmd.is_allow_missing_positional_set() {
            for pos in self
                .cmd
                .get_positionals()
                .filter(|a| !matcher.check_explicit(a.get_id(), &ArgPredicate::IsPresent))
            {
                if pos.get_index() < Some(highest_index) {
                    debug!(
                        "Validator::validate_required:iter: Missing {:?}",
                        pos.get_id()
                    );
                    missing_required.push(pos.get_id().clone());
                }
            }
        }

        if !missing_required.is_empty() {
            ok!(self.missing_required_error(matcher, missing_required));
        }

        Ok(())
    }

    fn is_missing_required_ok(
        &self,
        a: &Arg,
        matcher: &ArgMatcher,
        conflicts: &mut Conflicts,
    ) -> bool {
        debug!("Validator::is_missing_required_ok: {}", a.get_id());
        let conflicts = conflicts.gather_conflicts(self.cmd, matcher, a.get_id());
        !conflicts.is_empty()
    }

    // Failing a required unless means, the arg's "unless" wasn't present, and neither were they
    fn fails_arg_required_unless(&self, a: &Arg, matcher: &ArgMatcher) -> bool {
        debug!("Validator::fails_arg_required_unless: a={:?}", a.get_id());
        let exists = |id| matcher.check_explicit(id, &ArgPredicate::IsPresent);

        (a.r_unless_all.is_empty() || !a.r_unless_all.iter().all(exists))
            && !a.r_unless.iter().any(exists)
    }

    // `req_args`: an arg to include in the error even if not used
    fn missing_required_error(
        &self,
        matcher: &ArgMatcher,
        raw_req_args: Vec<Id>,
    ) -> ClapResult<()> {
        debug!("Validator::missing_required_error; incl={:?}", raw_req_args);
        debug!(
            "Validator::missing_required_error: reqs={:?}",
            self.required
        );

        let usg = Usage::new(self.cmd).required(&self.required);

        let req_args = {
            #[cfg(feature = "usage")]
            {
                usg.get_required_usage_from(&raw_req_args, Some(matcher), true)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            }

            #[cfg(not(feature = "usage"))]
            {
                raw_req_args
                    .iter()
                    .map(|id| {
                        if let Some(arg) = self.cmd.find(id) {
                            arg.to_string()
                        } else if let Some(_group) = self.cmd.find_group(id) {
                            self.cmd.format_group(id).to_string()
                        } else {
                            debug_assert!(false, "id={:?} is unknown", id);
                            "".to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
            }
        };

        debug!(
            "Validator::missing_required_error: req_args={:#?}",
            req_args
        );

        let used: Vec<Id> = matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
            .filter(|n| {
                // Filter out the args we don't want to specify.
                self.cmd.find(n).map_or(false, |a| !a.is_hide_set())
            })
            .cloned()
            .chain(raw_req_args)
            .collect();

        Err(Error::missing_required_argument(
            self.cmd,
            req_args,
            usg.create_usage_with_title(&used),
        ))
    }
}

#[derive(Default, Clone, Debug)]
struct Conflicts {
    potential: FlatMap<Id, Vec<Id>>,
}

impl Conflicts {
    fn new() -> Self {
        Self::default()
    }

    fn gather_conflicts(&mut self, cmd: &Command, matcher: &ArgMatcher, arg_id: &Id) -> Vec<Id> {
        debug!("Conflicts::gather_conflicts: arg={:?}", arg_id);
        let mut conflicts = Vec::new();
        for other_arg_id in matcher
            .arg_ids()
            .filter(|arg_id| matcher.check_explicit(arg_id, &ArgPredicate::IsPresent))
        {
            if arg_id == other_arg_id {
                continue;
            }

            if self
                .gather_direct_conflicts(cmd, arg_id)
                .contains(other_arg_id)
            {
                conflicts.push(other_arg_id.clone());
            }
            if self
                .gather_direct_conflicts(cmd, other_arg_id)
                .contains(arg_id)
            {
                conflicts.push(other_arg_id.clone());
            }
        }
        debug!("Conflicts::gather_conflicts: conflicts={:?}", conflicts);
        conflicts
    }

    fn gather_direct_conflicts(&mut self, cmd: &Command, arg_id: &Id) -> &[Id] {
        self.potential.entry(arg_id.clone()).or_insert_with(|| {
            let conf = if let Some(arg) = cmd.find(arg_id) {
                let mut conf = arg.blacklist.clone();
                for group_id in cmd.groups_for_arg(arg_id) {
                    let group = cmd.find_group(&group_id).expect(INTERNAL_ERROR_MSG);
                    conf.extend(group.conflicts.iter().cloned());
                    if !group.multiple {
                        for member_id in &group.args {
                            if member_id != arg_id {
                                conf.push(member_id.clone());
                            }
                        }
                    }
                }

                // Overrides are implicitly conflicts
                conf.extend(arg.overrides.iter().cloned());

                conf
            } else if let Some(group) = cmd.find_group(arg_id) {
                group.conflicts.clone()
            } else {
                debug_assert!(false, "id={:?} is unknown", arg_id);
                Vec::new()
            };
            debug!(
                "Conflicts::gather_direct_conflicts id={:?}, conflicts={:?}",
                arg_id, conf
            );
            conf
        })
    }
}

pub(crate) fn get_possible_values_cli(a: &Arg) -> Vec<PossibleValue> {
    if !a.is_takes_value_set() {
        vec![]
    } else {
        a.get_value_parser()
            .possible_values()
            .map(|pvs| pvs.collect())
            .unwrap_or_default()
    }
}
