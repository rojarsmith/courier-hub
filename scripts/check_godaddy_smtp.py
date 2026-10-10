#!/usr/bin/env python3
"""Check GoDaddy SMTP interactively without saving credentials or using Courier Hub."""

import argparse
import getpass
import smtplib
import ssl
import sys
import warnings
from email.message import EmailMessage
from email.utils import formatdate, make_msgid, parseaddr


HOST = "smtpout.secureserver.net"
TIMEOUT_SECONDS = 15


def read_address(prompt):
    value = input(prompt).strip()
    if (
        value.count("@") != 1
        or not all(value.split("@"))
        or any(char.isspace() or ord(char) < 32 or ord(char) == 127 for char in value)
        or parseaddr(value)[1] != value
    ):
        raise ValueError("Enter one bare email address without a display name.")
    return value


def ehlo(smtp):
    code, response = smtp.ehlo()
    if code != 250:
        raise smtplib.SMTPHeloError(code, response)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--host",
        default=HOST,
        help="SMTP DNS hostname (default: smtpout.secureserver.net)",
    )
    parser.add_argument(
        "--tls",
        choices=("implicit", "starttls"),
        default="implicit",
        help="implicit TLS on port 465 (default), or required STARTTLS on port 587",
    )
    parser.add_argument(
        "--send",
        action="store_true",
        help="also send one real test email to a recipient entered interactively",
    )
    args = parser.parse_args(argv)
    labels = args.host.split(".")
    if (
        not args.host
        or len(args.host) > 253
        or any(
            not label
            or len(label) > 63
            or label.startswith("-")
            or label.endswith("-")
            or any(not (char.isascii() and (char.isalnum() or char == "-")) for char in label)
            for label in labels
        )
    ):
        parser.error("--host must be a DNS hostname without a scheme, port, or path")
    if not sys.stdin.isatty():
        print(
            "Run in an interactive terminal; credentials are never read from files or pipes.",
            file=sys.stderr,
        )
        return 2

    stage = "Input"
    accepted = False
    try:
        username = read_address("Full mailbox address: ")
        # Abort rather than fall back to visible password input when no secure terminal exists.
        with warnings.catch_warnings():
            warnings.simplefilter("error", getpass.GetPassWarning)
            password = getpass.getpass("Mailbox password (hidden): ")
        if not password:
            raise ValueError("Mailbox password must not be empty.")
        recipient = (
            read_address("Test recipient you control (a real email will be sent): ")
            if args.send
            else None
        )

        stage = "Connection/TLS"
        context = ssl.create_default_context()
        port = 465 if args.tls == "implicit" else 587
        print(f"Connecting to {args.host}:{port} ({args.tls})...")
        if args.tls == "implicit":
            smtp = smtplib.SMTP_SSL(args.host, port, timeout=TIMEOUT_SECONDS, context=context)
        else:
            smtp = smtplib.SMTP(args.host, port, timeout=TIMEOUT_SECONDS)
        with smtp:
            ehlo(smtp)
            if args.tls == "starttls":
                smtp.starttls(context=context)
                ehlo(smtp)
            print("TLS OK:", smtp.sock.version())

            stage = "Authentication"
            code, _ = smtp.login(username, password)
            del password
            print(f"Authentication OK (SMTP {code}).")

            if recipient:
                message = EmailMessage()
                message["From"] = username
                message["To"] = recipient
                message["Subject"] = "Courier Hub SMTP connection test"
                message["Date"] = formatdate(localtime=False, usegmt=True)
                message["Message-ID"] = make_msgid()
                message.set_content("Independent SMTP test from Courier Hub diagnostic script.")
                stage = "Send"
                smtp.send_message(message, from_addr=username, to_addrs=[recipient])
                accepted = True
                print("SMTP accepted the test message. Check the inbox, spam folder, and bounces.")
            else:
                print("Authentication check completed; no email sent.")
            stage = "QUIT"
        return 0
    except (EOFError, KeyboardInterrupt):
        print("\nCheck cancelled.", file=sys.stderr)
        return 130
    except getpass.GetPassWarning:
        print("Secure password input is unavailable. Run in an interactive terminal.", file=sys.stderr)
        return 2
    except ValueError:
        print("Invalid input: use a nonempty password and one bare email address per prompt.", file=sys.stderr)
        return 2
    except smtplib.SMTPAuthenticationError as exc:
        print(f"{stage} failed: {type(exc).__name__}, SMTP {exc.smtp_code}.", file=sys.stderr)
        print(
            "Check that the selected SMTP host belongs to your mailbox provider "
            "and use the mailbox password.",
            file=sys.stderr,
        )
    except smtplib.SMTPResponseException as exc:
        print(f"{stage} failed: {type(exc).__name__}, SMTP {exc.smtp_code}.", file=sys.stderr)
    except (smtplib.SMTPException, OSError) as exc:
        # Raw server responses and exception text can contain mailbox addresses or other private data.
        print(f"{stage} failed: {type(exc).__name__}. Check the README troubleshooting table.", file=sys.stderr)
    if accepted:
        print("The message was already accepted before disconnecting; check delivery before retrying.", file=sys.stderr)
    elif stage == "Send":
        print("Delivery may be uncertain; check the recipient/provider records before retrying.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
