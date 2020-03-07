# sqstransfer
![Rust](https://github.com/ohke/sqstransfer/workflows/Rust/badge.svg?branch=master&event=push)

CLI tool that transfers Amazon SQS messages to another SQS.

## Installation

### Binaries
Download from [release page](https://github.com/ohke/sqstransfer/releases).

## Usage
```
$ sqstransfer \
  --source https://sqs.us-west-2.amazonaws.com/123456789012/your-sqs-dlq \ 
  --destination https://sqs.us-west-2.amazonaws.com/123456789012/your-sqs
```

You need to set AWS environment variables before execution.
- AWS_ACCESS_KEY_ID
- AWS_SECRET_ACCESS_KEY
- AWS_DEFAULT_REGION (or use `--region` option)

## License
sqstransfer is distributed under the terms of the MIT license.

See [LICENSE](https://github.com/ohke/sqstransfer/blob/master/LICENSE) for details.