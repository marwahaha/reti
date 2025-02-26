extern crate chrono;
//extern crate rustc_serialize;
extern crate reti_printing;
extern crate reti_storage;
extern crate tempfile;

extern crate config;
extern crate xdg;

#[macro_use]
extern crate clap;

#[macro_use]
extern crate failure;

mod cli;
mod utils;

use chrono::*;
use clap::{ArgMatches, Shell};
use failure::Error;
use reti_printing::printer;
use reti_storage::data;
use reti_storage::legacy_parser;
use std::env;
use std::io;
use std::io::prelude::*;
use std::io::BufReader;
use std::process::{exit, Command};

fn get_settings() -> Result<config::Config, Error> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix("reti")?;
    let config_path = match xdg_dirs.find_config_file("reti.toml") {
        Some(x) => x,
        None => return Err(format_err!("Unable to open reti.toml")),
    };
    //.expect("Unable to open reti.toml")?;

    let mut settings = config::Config::default();
    settings.merge(config::File::from(config_path))?;
    Ok(settings)
}

fn main() {
    let args = cli::build_cli().get_matches();

    if let Some(ref matches) = args.subcommand_matches("completions") {
        let shell = matches.value_of("SHELL").unwrap();
        cli::build_cli().gen_completions_to(
            "reti",
            shell.parse::<Shell>().unwrap(),
            &mut io::stdout(),
        );
        exit(0)
    }

    let mut pretty_json = args.is_present("save-pretty");

    if let Some(ref matches) = args.subcommand_matches("init") {
        subcmd_init(matches, pretty_json);
        return;
    }

    let mut storage_file = String::new();
    if let Ok(settings) = get_settings() {
        if let Ok(f) = settings.get_str("storage-file") {
            storage_file = f;
        }
        if let Ok(p) = settings.get_bool("save-pretty") {
            pretty_json = p;
        }
    }

    if let Ok(f) = value_t!(args, "file", String) {
        storage_file = f;
    }
    println!("Use storage_file: {}", storage_file);

    let mut store = match data::Storage::from_file(&storage_file) {
        Ok(store) => store,
        Err(e) => {
            println!("{:?}", e);
            exit(-1);
        }
    };

    if let Some(ref matches) = args.subcommand_matches("show") {
        subcmd_show(&store, matches);
    }

    let mut do_write = false;
    if let Some(ref matches) = args.subcommand_matches("import") {
        subcmd_import(&mut store, matches);
        do_write = true;
    }

    if let Some(ref matches) = args.subcommand_matches("get") {
        subcmd_get(&mut store, matches)
    }

    if let Some(ref matches) = args.subcommand_matches("set") {
        if subcmd_set(&mut store, matches) {
            do_write = true;
        } else {
            println!("Setting did not succeed, nothing will be saved!");
        }
    }

    if let Some(ref matches) = args.subcommand_matches("rm") {
        if subcmd_remove(&mut store, matches) {
            do_write = true;
        } else {
            println!("Removal failed, nothing will be saved!");
        }
    }

    if let Some(ref matches) = args.subcommand_matches("add") {
        if subcmd_add(&mut store, matches) {
            do_write = true;
        } else {
            println!("Add did not work, nothing will be saved!");
        }
    }

    if let Some(ref matches) = args.subcommand_matches("edit") {
        if subcmd_edit(&mut store, matches) {
            do_write = true;
        } else {
            println!("Edit canceled, nothing will be saved!");
        }
    }

    if do_write {
        if !store.save(&storage_file, pretty_json) {
            println!("Unable to write file: {}", &storage_file);
        }
    }
}

fn subcmd_import(store: &mut data::Storage, matches: &ArgMatches) {
    let leg_file = value_t!(matches, "legacy_file", String).unwrap_or_else(|e| e.exit());

    if !store.import_legacy(&leg_file) {
        println!("Unable to import data!")
    }
}

fn subcmd_remove(store: &mut data::Storage, matches: &ArgMatches) -> bool {
    let dates = values_t!(matches, "dates", String).unwrap_or(vec![]);

    let force = matches.is_present("force");
    let dates = dates.iter().filter_map(|d| legacy_parser::parse_date(&d));

    let mut removed = false;

    for date in dates {
        if !force {
            let q = format!("Really remove {} from store? [y/N] ", date);
            match utils::yes_no(q.as_ref(), utils::YesNoAnswer::NO) {
                utils::YesNoAnswer::NO => {
                    println!("Skip removal of {}.", date);
                    continue;
                }
                utils::YesNoAnswer::YES => (),
            }
        }

        if store.remove_day_nd(date) {
            removed = true;
            println!("{} has been removed!", date);
        } else {
            println!("{} doesn't exist!", date);
        }
    }
    removed
}

fn subcmd_edit(store: &mut data::Storage, matches: &ArgMatches) -> bool {
    let p_dates = values_t!(matches, "dates", String).unwrap_or(vec![]);

    let dates = p_dates
        .iter()
        .filter_map(|d| legacy_parser::parse_date(&d))
        .filter_map(|d| store.get_day(d.year() as u16, d.month() as u8, d.day() as u8))
        .map(|d| d.as_legacy())
        .collect::<Vec<String>>();

    let mut s = dates.join("\n");
    if s.is_empty() {
        let today = Utc::today().naive_local();
        if let Some(day) =
            store.get_day(today.year() as u16, today.month() as u8, today.day() as u8)
        {
            s.push_str(&day.as_legacy())
        } else {
            s.push_str("# Lines starting with '#' will be ignored\n");
            s.push_str("# Default date is today!\n");
            s.push_str("# Date         Parts w/o and w/ factor (0.5)  Comment\n");
            s.push_str("# 2016-04-25   08:00-12:00  13:00-17:00-0.5   # coment\n");
            let today = data::Day::new_today();
            s.push_str(&today.as_legacy())
        }
    }

    let mut file = tempfile::NamedTempFile::new().unwrap();
    let _ = write!(file, "{}\n", &s);

    match Command::new(env::var("EDITOR").unwrap_or("vim".to_string()))
        .arg(file.path().to_str().unwrap())
        .status()
    {
        Err(e) => {
            println!("Error occured: '{:?}'", e);
            return false;
        }
        Ok(x) => {
            if !x.success() {
                println!("Editor exit was failure!");
                return false;
            }
        }
    }

    let mut f = BufReader::new(file);
    let _ = f.seek(std::io::SeekFrom::Start(0));

    for (i, line) in f.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            println!("ignore empty line: {}", line);
            continue;
        }

        let day = match legacy_parser::parse_line(&line) {
            Ok(day) => day,
            Err(e) => {
                println!("ignore {}: '{}' {:?}", i, line, e);
                continue;
            }
        };

        store.add_day_force(day);
    }

    return true;
}

fn subcmd_get(store: &data::Storage, matches: &ArgMatches) {
    if let Some(ref _matches) = matches.subcommand_matches("fee") {
        println!("Current fee: {}", store.get_fee());
    }
}

fn subcmd_set(store: &mut data::Storage, matches: &ArgMatches) -> bool {
    if let Some(ref matches) = matches.subcommand_matches("fee") {
        let fee = value_t!(matches, "value", f32).unwrap_or_else(|e| e.exit());
        store.set_fee(fee);
        return true;
    }
    return false;
}

fn subcmd_add(store: &mut data::Storage, matches: &ArgMatches) -> bool {
    if let Some(ref matches) = matches.subcommand_matches("part") {
        let start = value_t!(matches, "start", String).unwrap_or_else(|e| e.exit());
        let start = legacy_parser::parse_time(&start);
        if start.is_none() {
            println!("Unable to parse start as time: format HH:MM or HHMM");
            return false;
        }
        let start = start.unwrap();
        let mut part = data::Part {
            start: start,
            stop: None,
            factor: None,
        };

        if let Ok(stop) = value_t!(matches, "stop", String) {
            part.stop = legacy_parser::parse_time(&stop); // { Some(stop);
        }

        let date = Utc::today().naive_local();
        return store.add_part(date, part);
    }

    if let Some(ref matches) = matches.subcommand_matches("parse") {
        let data = values_t!(matches, "data", String).unwrap_or_else(|e| e.exit());

        match legacy_parser::parse_line(&data.join(" ")) {
            Ok(day) => return store.add_day(day),
            Err(_) => {
                println!("Unable to parse data");
                return false;
            }
        }
    }
    return false;
}

fn subcmd_init(matches: &ArgMatches, pretty: bool) {
    let mut store = data::Storage::new();

    let storage_file =
        value_t!(matches, "storage_file", String).unwrap_or("times.json".to_string());

    if let Ok(leg_file) = value_t!(matches, "legacy_file", String) {
        if !store.import_legacy(&leg_file) {
            println!("Unable to import data!");
        }
    }

    if !store.save(&storage_file, pretty) {
        println!("Unable to write file: {}", &storage_file);
        exit(-1);
    }

    println!("New store has been created: {}", storage_file);
}

fn subcmd_show(store: &data::Storage, matches: &ArgMatches) {
    let show_days = matches.is_present("days");
    let mut worked = matches.is_present("worked");
    let breaks = matches.is_present("breaks");
    let verbose = matches.is_present("verbose");
    let parts = matches.is_present("parts");
    let today = chrono::Utc::today();

    if !breaks {
        worked = true;
    }

    if let Some(ref matches) = matches.subcommand_matches("year") {
        let vals_num: Vec<u16> = if matches.is_present("years") {
            values_t!(matches, "years", u16).unwrap_or_else(|e| e.exit())
        } else {
            let c = today.year() as u16;
            if verbose {
                println!("Assume current year: {}", c);
            }
            vec![c]
        };

        let mut vals: Vec<&data::Year> = vec![];
        for y in vals_num {
            match store.get_year(y) {
                Some(y) => vals.push(y),
                None => {
                    println!("Year {} not available!", y);
                }
            }
        }

        let p = printer::Printer::with_years(vals)
            .set_fee(store.get_fee())
            .show_days(show_days)
            .show_worked(worked)
            .show_breaks(breaks)
            .show_parts(parts)
            .show_verbose(verbose);
        print!("{}", p);
    }

    if let Some(ref matches) = matches.subcommand_matches("month") {
        let y = if matches.is_present("year") {
            value_t!(matches, "year", u16).unwrap_or_else(|e| e.exit())
        } else {
            today.year() as u16
        };

        let vals_num: Vec<u8> = if matches.is_present("months") {
            values_t!(matches, "months", u8).unwrap_or_else(|e| e.exit())
        } else {
            let c = today.month() as u8;
            if verbose {
                println!("Assume current month: {}", c);
            }
            vec![c]
        };

        let mut vals: Vec<data::Month> = vec![];
        for x in vals_num {
            match store.get_month(y, x) {
                Some(x) => vals.push(x),
                None => {
                    println!("Month {} not available for year {}!", x, y);
                }
            }
        }
        if vals.len() == 0 {
            return;
        }
        let p = printer::Printer::with_months(vals)
            .set_fee(store.get_fee())
            .show_days(show_days)
            .show_worked(worked)
            .show_breaks(breaks)
            .show_parts(parts)
            .show_verbose(verbose);
        print!("{}", p);
        return;
    }

    if let Some(ref matches) = matches.subcommand_matches("week") {
        let y = if matches.is_present("year") {
            value_t!(matches, "year", u16).unwrap_or_else(|e| e.exit())
        } else {
            today.year() as u16
        };

        let vals_num: Vec<u32> = if matches.is_present("weeks") {
            values_t!(matches, "weeks", u32).unwrap_or_else(|e| e.exit())
        } else {
            let c = today.iso_week().week();
            if verbose {
                println!("Assume current week: {}", c);
            }
            vec![c]
        };

        let mut vals: Vec<data::Week> = vec![];
        for x in vals_num {
            match store.get_week(y, x) {
                Some(x) => vals.push(x),
                None => {
                    println!("Week {} not available for year {}!", x, y);
                }
            }
        }
        if vals.len() == 0 {
            return;
        }
        let p = printer::Printer::with_weeks(vals)
            .set_fee(store.get_fee())
            .show_days(show_days)
            .show_worked(worked)
            .show_breaks(breaks)
            .show_parts(parts)
            .show_verbose(verbose);
        print!("{}", p);
    }

    if let Some(ref matches) = matches.subcommand_matches("day") {
        let y = if matches.is_present("year") {
            value_t!(matches, "year", u16).unwrap_or_else(|e| e.exit())
        } else {
            today.year() as u16
        };

        let m = if matches.is_present("month") {
            value_t!(matches, "month", u8).unwrap_or_else(|e| e.exit())
        } else {
            today.month() as u8
        };

        let vals_num: Vec<u8> = if matches.is_present("days") {
            values_t!(matches, "days", u8).unwrap_or_else(|e| e.exit())
        } else {
            let c = today.day() as u8;
            if verbose {
                println!("Assume current day: {}", c);
            }
            vec![c]
        };

        let mut vals: Vec<&data::Day> = vec![];
        for x in vals_num {
            match store.get_day(y, m, x) {
                Some(x) => vals.push(x),
                None => {
                    println!("Day {} not available for month {} in year {}!", x, m, y);
                }
            }
        }

        let p = printer::Printer::with_days(vals)
            .show_worked(worked)
            .show_breaks(breaks)
            .show_parts(parts)
            .show_verbose(verbose);
        print!("{}", p);
        return;
    }
}
