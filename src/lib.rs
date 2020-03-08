extern crate clap;

use std::{thread, env, fmt, process};
use std::collections::HashMap;
use std::sync::{mpsc};
use clap::{App, Arg};
use rusoto_sqs::{
    SqsClient, Sqs, Message, ReceiveMessageRequest,
    SendMessageBatchRequest, SendMessageBatchRequestEntry, 
    DeleteMessageBatchRequest, DeleteMessageBatchRequestEntry
};
use std::thread::JoinHandle;

pub struct Config {
    source: String,
    destination: String,
    threads: usize,
    region: String,
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f, "(source:{}, destination:{}, threads:{}, region:{})", 
            self.source, self.destination, self.threads, self.region
        )
    }
}

pub fn parse() -> Config {
    let app = App::new("sqstransfer")
        .version("0.1.0")
        .about("CLI tool that transfers Amazon SQS messages to another SQS.")
        .arg(
            Arg::with_name("source")
                .help("Source SQS url")
                .short("s")
                .long("source")
                .takes_value(true)
                .required(true)
        )
        .arg(
            Arg::with_name("destination")
                .help("Destination SQS url")
                .short("d")
                .long("destination")
                .takes_value(true)
                .required(false)
        )
        .arg(
            Arg::with_name("region")
                .help("AWS region url")
                .short("r")
                .long("region")
                .takes_value(true)
                .required(false)
        )
        .arg(
            Arg::with_name("delete")
                .help("Delete messages")
                .long("delete")
                .takes_value(false)
                .required(false)
        )
        .arg(
            Arg::with_name("threads")
                .help("Number of threads")
                .short("t")
                .long("threads")
                .takes_value(true)
                .required(false)
                .default_value("16")
        );

    let matches = app.get_matches();

    let source = matches.value_of("source").unwrap().to_string();

    let destination = match matches.value_of("destination") {
        Some(destination) => destination.to_string(),
        None => "".to_string(),
    };
    let has_delete = matches.is_present("delete");
    if (destination != "" && has_delete) || (destination == "" && !has_delete) {
        eprintln!("Set `--destination` or `--delete`.");
        process::exit(1);
    }

    let threads = matches.value_of("threads").unwrap().parse::<usize>().unwrap();

    let default_region = &env::var("AWS_DEFAULT_REGION").unwrap_or("".to_string());
    let region = match matches.value_of("region") {
        Some(region) => region.to_string(),
        None => default_region.to_string(),
    };
    if region == "" {
        eprintln!("Set `--region` option or AWS_DEFAULT_REGION environment value.");
        process::exit(1);
    }

    Config {source, destination, threads, region}
}

pub fn run(config: &Config) {
    let mut transferer = Transferer::new(&config.source, &config.destination, config.threads, &config.region);

    let message_count = transferer.execute();

    println!(
        "Transferred messages count: {} (from: {}, to: {}) ",
        message_count, &config.source, &config.destination
    );
}

struct Transferer {
    handles: Vec<Option<JoinHandle<usize>>>,
    senders: Vec<mpsc::Sender<TransfererCommand>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TransfererError {
    Dequeue,
    Enqueue,
    Delete,
}

enum TransfererCommand {
    Terminate,
}

impl Transferer {
    fn new(source: &str, destination: &str, threads: usize, region: &str) -> Transferer {
        let mut handles = vec![];
        let mut senders = vec![];

        let region: rusoto_core::Region = match region.parse() {
            Ok(region) => region,
            Err(_) => {
                eprintln!("Invalid region identifier: {}", region);
                process::exit(1);
            }
        };
        let client = SqsClient::new(region);

        for _ in 0..threads {
            let source = source.to_string();
            let destination = destination.to_string();
            let client = client.clone();

            let (sender, receiver) = mpsc::channel();
            senders.push(sender);

            let handle = thread::spawn(move || {
                // TODO: async/await (rusoto 0.43.0~)
                let mut message_count :usize = 0;
                loop {
                    match receiver.try_recv() {
                        Ok(TransfererCommand::Terminate) => {
                            break;
                        },
                        _ => {}
                    }

                    let n = match transfer_message(&client, &source, &destination) {
                        Ok(n) => n,
                        Err(_) => {
                            break;
                        },
                    };

                    if n <= 0 {
                        break;
                    }

                    message_count += n;
                }

                message_count
            });

            handles.push(Some(handle));
        }

        Transferer {
            handles,
            senders,
        }
    }

    fn execute(&mut self) -> usize {
        (&mut self.handles).into_iter().map(|h| {
            if let Some(handle) = h.take() {
                handle.join().unwrap()
            } else {
                0
            }
        }).sum()
    }
}

impl Drop for Transferer {
    fn drop(&mut self) {
        for sender in &mut self.senders {
            let _ = sender.send(TransfererCommand::Terminate);
        }

        (&mut self.handles).into_iter().for_each(|h| {
            if let Some(handle) = h.take() {
                handle.join().unwrap();
            }
        });
    }
}

fn transfer_message(client: &SqsClient, source: &str, destination: &str) -> Result<usize, TransfererError> {
    let messages = dequeue(client, source)?;

    if messages.len() >= 1 {
        if destination != "" {
            let handles = enqueue(client, &messages, destination)?;
            delete(client, &handles, source)?;
        } else {
            let mut handles: Vec<String> = vec![];
            for message in &messages {
                handles.push(match &message.receipt_handle {
                    Some(receipt_handle) => receipt_handle.to_string(),
                    None => "".to_string(),
                });
            }

            delete(client, &handles, source)?;
        }
    }

    Ok(messages.len())
}

fn dequeue(client: &SqsClient, source: &str) -> Result<Vec<Message>, TransfererError> {
    match client.receive_message(ReceiveMessageRequest {
        queue_url: source.to_string(),
        max_number_of_messages: Some(10),
        // TODO: https://github.com/rusoto/rusoto/issues/1444 (rusoto 0.44~)
        // message_attribute_names: Some(vec!["key.*".to_string()]),
        ..Default::default()
    }).sync() {
        Ok(result) => Ok(match result.messages {
            Some(messages) => messages,
            None => vec![],
        }),
        Err(e) => {
            eprintln!("Dequeue error: {:?}", e);
            Err(TransfererError::Dequeue)
        }
    }
}

fn enqueue(client: &SqsClient, messages: &Vec<Message>, destination: &str) -> Result<Vec<String>, TransfererError> {
    let mut entries: Vec<SendMessageBatchRequestEntry> = vec![];
    let mut handle_map = HashMap::new();

    for (i, message) in messages.iter().enumerate() {
        entries.push(SendMessageBatchRequestEntry {
            id: i.to_string(),
            message_body: match &message.body {
                Some(body) => body.to_string(),
                None => "".to_string(),
            },
            message_attributes: match &message.message_attributes {
                Some(message_attributes) => Some(message_attributes.clone()),
                None => None,
            },
            ..Default::default()
        });

        handle_map.insert(i.to_string(), match &message.receipt_handle {
            Some(receipt_handle) => receipt_handle.to_string(),
            None => {
                eprintln!("Enqueue error: be able to get `ReceiptHandle`.");
                return Err(TransfererError::Enqueue);
            }
        });
    }

    match client.send_message_batch(SendMessageBatchRequest {
        queue_url: destination.to_string(),
        entries,
    })
    .sync() {
        Ok(result) => {
            for f in result.failed {
                handle_map.remove(&f.id);
            }

            let mut handles = vec![];
            for (_, v) in handle_map.iter() {
                handles.push(v.to_string());
            }

            Ok(handles)
        },
        Err(e) => {
            eprintln!("Enqueue error: {:?}", e);
            Err(TransfererError::Enqueue)
        }
    }
}

fn delete(client: &SqsClient, handles: &Vec<String>, source: &str) -> Result<(), TransfererError> {
    let mut entries: Vec<DeleteMessageBatchRequestEntry> = vec![];
    
    for (i, handle) in handles.iter().enumerate() {
        entries.push(DeleteMessageBatchRequestEntry {
            id: i.to_string(),
            receipt_handle: handle.to_string(),
        });
    }

    match client.delete_message_batch(DeleteMessageBatchRequest {
        queue_url: source.to_string(),
        entries,
    })
    .sync() {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Delete error: {:?}", e);
            Err(TransfererError::Delete)
        }
    }
}