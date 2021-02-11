use chrono::{DateTime, Local};
use needletail::{parse_fastx_file, Sequence};
use plotters::prelude::*;
use serde_json::json;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::io::Write;
use std::path::Path;
use tera::{self, Context, Tera};

pub(crate) fn process<P: AsRef<Path>>(filename: P, k: u8) -> Result<(), Box<dyn Error>> {
    let mut mean_read_qualities = HashMap::new();
    let mut base_count = HashMap::new();
    let mut base_quality_count = HashMap::new();
    let mut read_lengths = HashMap::new();
    let mut kmers = HashMap::new();
    let mut gc_content = Vec::new();
    let mut reader = parse_fastx_file(&filename).expect("valid path/file");

    // Gather data from every record
    while let Some(record) = reader.next() {
        let seqrec = record.expect("invalid record");

        let sequence = seqrec.seq();
        let count_read_length = read_lengths.entry(sequence.len()).or_insert_with(|| 0);
        *count_read_length += 1;

        let gc: Vec<_> = sequence
            .iter()
            .filter(|b| b == &&b'G' || b == &&b'C')
            .collect();
        let without_n: Vec<_> = sequence.iter().filter(|b| b != &&b'N').collect();
        gc_content.push((gc.len() * 100) / without_n.len());

        let mean_read_quality = if let Some(qualities) = seqrec.qual() {
            qualities.iter().map(|q| (q - 33) as u64).sum::<u64>() / qualities.len() as u64
        } else {
            0
        };

        let count_qual = mean_read_qualities
            .entry(mean_read_quality)
            .or_insert_with(|| 0);
        *count_qual += 1;

        if let Some(qualities) = seqrec.qual() {
            for (pos, q) in qualities.iter().enumerate() {
                let rec = base_quality_count.entry(pos).or_insert_with(Vec::new);
                rec.push(q - 33);
            }
        }

        for (pos, base) in sequence.iter().enumerate() {
            let pos_count = base_count.entry(pos).or_insert_with(|| {
                "ACGTN"
                    .chars()
                    .map(|c| (c, 0_u64))
                    .collect::<HashMap<_, _>>()
            });
            let count = pos_count.get_mut(&(*base as char)).expect("Invalid base");
            *count += 1;
        }

        let norm_seq = seqrec.normalize(false);
        let rc = norm_seq.reverse_complement();
        for (_, kmer, _) in norm_seq.canonical_kmers(k, &rc) {
            let count = kmers.entry(kmer.to_owned()).or_insert_with(|| 0_u64);
            *count += 1;
        }
    }

    // Data for base per position
    let mut base_count_data = Vec::new();
    for (position, bases) in base_count {
        for (base, count) in bases {
            base_count_data.push(json!({"pos": position, "count": count, "base": base}));
        }
    }

    let mut bpp_specs: Value =
        serde_json::from_str(include_str!("report/base_per_pos_specs.json"))?;
    bpp_specs["data"]["values"] = json!(base_count_data);

    // Data for read lengths
    let mut read_length_data = Vec::new();
    for (length, count) in read_lengths {
        read_length_data.push(json!({"length": length, "count": count}));
    }

    let mut rle_specs: Value =
        serde_json::from_str(include_str!("report/read_lengths_specs.json"))?;
    rle_specs["data"]["values"] = json!(read_length_data);

    // Data for mean read qualities
    let mut mean_read_quality_data = Vec::new();
    for (quality, count) in mean_read_qualities {
        mean_read_quality_data.push(json!({"score": quality, "count": count}))
    }

    let mut sqc_specs: Value =
        serde_json::from_str(include_str!("report/sequence_quality_score_specs.json"))?;
    sqc_specs["data"]["values"] = json!(mean_read_quality_data);

    // Data for kmer quantities
    let mut kmer_data = Vec::new();
    for (kmer, count) in kmers {
        kmer_data.push(json!({"k_mer": std::str::from_utf8(&kmer).unwrap(), "count": count}))
    }

    let mut counter_specs: Value = serde_json::from_str(include_str!("report/counter_specs.json"))?;
    counter_specs["data"]["values"] = json!(kmer_data);

    // Data for GC content
    let mut gc_data = Vec::new();
    let mut temp_gc = HashMap::new();
    for content in gc_content {
        let count = temp_gc.entry(content).or_insert_with(|| 0_u64);
        *count += 1;
    }
    for (perc, count) in temp_gc {
        gc_data.push(json!({"gc_pct": perc, "count": count, "type": "gc"}))
    }

    let mut gc_specs: Value = serde_json::from_str(include_str!("report/gc_content_specs.json"))?;
    gc_specs["data"]["values"] = json!(gc_data);

    // Data for base quality per position
    let mut base_per_pos_data = Vec::new();
    for (position, qualities) in base_quality_count {
        let quartiles = Quartiles::new(&qualities);
        let avg = qualities.iter().map(|q| *q as f64).sum::<f64>() / qualities.len() as f64;
        let values = quartiles.values();
        base_per_pos_data.push(json!({
        "pos": position,
        "average": avg,
        "upper": values.get(4).unwrap(),
        "lower": values.get(0).unwrap(),
        "q1": values.get(1).unwrap(),
        "q3": values.get(3).unwrap(),
        "median":values.get(2).unwrap(),
        }));
    }

    let mut qpp_specs: Value =
        serde_json::from_str(include_str!("report/quality_per_pos_specs.json"))?;
    qpp_specs["data"]["values"] = json!(base_per_pos_data);

    let plots = json!({
        "k-mer quantities": {"short": "count", "specs": counter_specs.to_string()},
        "gc content": {"short": "gc", "specs": gc_specs.to_string()},
        "base sequence quality": {"short": "base", "specs": qpp_specs.to_string()},
        "sequence quality score": {"short": "qual", "specs": sqc_specs.to_string()},
        "base sequence content": {"short": "cont", "specs": bpp_specs.to_string()},
        "read lengths": {"short": "rlen", "specs": rle_specs.to_string()},
    });

    let meta = json!({
        "canonical": {"name": "canonical", "value": "True"},
        "k": {"name": "k", "value": k},
    });

    let mut templates = Tera::default();
    templates.register_filter("embed_source", embed_source);
    templates.add_raw_template("report.html.tera", include_str!("report/report.html.tera"))?;
    let mut context = Context::new();
    context.insert("plots", &plots);
    context.insert("meta", &meta);
    let local: DateTime<Local> = Local::now();
    context.insert("time", &local.format("%a %b %e %T %Y").to_string());
    context.insert("version", &env!("CARGO_PKG_VERSION"));
    let html = templates.render("report.html.tera", &context)?;
    io::stdout().write_all(html.as_bytes())?;
    Ok(())
}

fn embed_source(
    value: &tera::Value,
    _: &HashMap<String, tera::Value>,
) -> tera::Result<tera::Value> {
    let url = tera::try_get_value!("upper", "value", String, value);
    let source = reqwest::get(&url).unwrap().text().unwrap();
    Ok(tera::to_value(source).unwrap())
}
