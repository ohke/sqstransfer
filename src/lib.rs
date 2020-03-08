extern crate clap;

use std::{thread, env, fmt, process};
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
    senders: Vec<mpsc::Sender<Command>>,
}

enum Command {
    Terminate,
}

impl Transferer {
    fn new(source: &str, destination: &str, threads: usize, region: &str) -> Transferer {
        let mut handles = vec![];
        let mut senders = vec![];

        for _ in 0..threads {
            let source = source.to_string();
            let destination = destination.to_string();
            let region = region.to_string();

            let (sender, receiver) = mpsc::channel();
            senders.push(sender);

            let handle = thread::spawn(move || {
                // TODO: async/await (rusoto 0.43.0~)
                let client = SqsClient::new(region.parse().unwrap());

                let mut message_count :usize = 0;
                loop {
                    if receiver.try_recv().is_ok() {
                        break;
                    }

                    let n = transfer_message(&client, &source, &destination);
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
            let _ = sender.send(Command::Terminate);
        }

        (&mut self.handles).into_iter().for_each(|h| {
            if let Some(handle) = h.take() {
                handle.join().unwrap();
            }
        });
    }
}

fn transfer_message(client: &SqsClient, source: &str, destination: &str) -> usize {
    let messages = dequeue(client, source);
    
    if messages.len() >= 1 {
        if destination != "" {
            match enqueue(client, &messages, destination) {
                Ok(handles) => match delete(client, &handles, source) {
                    Ok(()) => (),
                    Err(error_message) => eprintln!("Failed to delete messages: {}", error_message),
                },
                Err(error_message) => eprintln!("Failed to enqueue messages: {}", error_message),
            };
        } else {
            let mut handles: Vec<String> = vec![];
            for message in &messages {
                handles.push(match &message.receipt_handle {
                    Some(receipt_handle) => receipt_handle.to_string(),
                    None => "".to_string(),
                });
            }

            match delete(client, &handles, source) {
                Ok(()) => (),
                Err(error_message) => eprintln!("Failed to delete messages: {}", error_message),
            };
        }
    }

    messages.len()
}

fn dequeue(client: &SqsClient, source: &str) -> Vec<Message> {
    let result = client.receive_message(ReceiveMessageRequest {
        queue_url: source.to_string(),
        max_number_of_messages: Some(10),
        // TODO: https://github.com/rusoto/rusoto/issues/1444 (rusoto 0.44~)
        // message_attribute_names: Some(vec!["key.*".to_string()]),
        ..Default::default()
    })
    .sync()
    .expect("Failed to dequeue messages.");

    match result.messages {
        Some(messages) => messages,
        None => vec![],
    }
}

fn enqueue(client: &SqsClient, messages: &Vec<Message>, destination: &str) -> Result<Vec<String>, String> {
    let mut entries: Vec<SendMessageBatchRequestEntry> = vec![];
    let mut handles: Vec<String> = vec![];

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

        handles.push(match &message.receipt_handle {
            Some(receipt_handle) => receipt_handle.to_string(),
            None => "".to_string(),
        });
    }

    let _result = client.send_message_batch(SendMessageBatchRequest {
        queue_url: destination.to_string(),
        entries,
    })
    .sync()
    .expect("Failed to send message.");

    Ok(handles)
}

fn delete(client: &SqsClient, handles: &Vec<String>, source: &str) -> Result<(), String> {
    let mut entries: Vec<DeleteMessageBatchRequestEntry> = vec![];
    
    for (i, handle) in handles.iter().enumerate() {
        entries.push(DeleteMessageBatchRequestEntry {
            id: i.to_string(),
            receipt_handle: handle.to_string(),
        });
    }

    let _result = client.delete_message_batch(DeleteMessageBatchRequest {
        queue_url: source.to_string(),
        entries,
    })
    .sync()
    .expect("Failed to delete message.");

    Ok(())
}