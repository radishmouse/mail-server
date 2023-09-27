
let "rto_raw" "to_lowercase(header.reply-to.raw)";
if eval "!is_empty(rto_raw)" {
    let "rto_addr" "to_lowercase(header.reply-to.addr)";
    let "rto_name" "to_lowercase(header.reply-to.name)";

    if eval "is_email(rto_addr)" {
        let "t.HAS_REPLYTO" "1";
        let "rto_domain" "domain_part(email_part(rto_addr, 'domain'), 'sld')";
        let "from_addr" "to_lowercase(header.from.addr)";
        let "from_domain" "domain_part(email_part(from_addr, 'domain'), 'sld')";

        if eval "eq_ignore_case(header.reply-to, header.from)" {
            let "t.REPLYTO_EQ_FROM" "1";
        } else {
            if eval "rto_domain == from_domain" {
                let "t.REPLYTO_DOM_EQ_FROM_DOM" "1";
            } else {
                let "is_from_list" "!is_empty(header.List-Unsubscribe:List-Id:X-To-Get-Off-This-List:X-List:Auto-Submitted[*])";
                if eval "!is_from_list && contains_ignore_case(header.to:cc:bcc[*].addr[*], rto_addr)"  {
                    let "t.REPLYTO_EQ_TO_ADDR" "1";
                } else {
                    let "t.REPLYTO_DOM_NEQ_FROM_DOM" "1";
                }

                if eval "!is_from_list &&
                         !eq_ignore_case(from_addr, header.to.addr) && 
                         !(count(envelope.to) == 1 && envelope.to[0] == from_addr)" {
                    let "i" "count(envelope.to)";
                    let "found_domain" "0";

                    while "i != 0" {
                        let "i" "i - 1";

                        if eval "domain_part(email_part(envelope.to[i], 'domain'), 'sld') == from_domain" {
                            let "found_domain" "1";
                            break;
                        }
                    }

                    if eval "!found_domain" {
                        let "t.SPOOF_REPLYTO" "1";
                    }
                }
            }

            if eval "!is_empty(rto_name) && eq_ignore_case(rto_name, header.from.name)" {
                let "t.REPLYTO_DN_EQ_FROM_DN" "1";
            }
        }

        if string :list "${rto_domain}" "spam/free-domains" {
            let "t.FREEMAIL_REPLYTO" "1";
            if allof(eval "rto_domain != from_domain", string :list "${from_domain}" "spam/free-domains") {
                let "t.FREEMAIL_REPLYTO_NEQ_FROM_DOM" "1";
            }

        } elsif string :list "${rto_domain}" "spam/disposable-domains" {
            let "t.DISPOSABLE_REPLYTO" "1";
        }

    } else {
        let "t.REPLYTO_UNPARSEABLE" "1";
    }

    if eval "is_ascii(header.reply-to) && contains(rto_raw, '=?') && contains(rto_raw, '?=')" {
        if eval "contains(rto_raw, '?q?')" {
            # Reply-To header is unnecessarily encoded in quoted-printable
            let "t.REPLYTO_EXCESS_QP" "1";
        } elsif eval "contains(rto_raw, '?b?')" {
            # Reply-To header is unnecessarily encoded in base64
            let "t.REPLYTO_EXCESS_BASE64" "1";
        }
    }

    if eval "contains(rto_name, 'mr. ') || contains(rto_name, 'ms. ') || contains(rto_name, 'mrs. ') || contains(rto_name, 'dr. ')" {
        let "t.REPLYTO_EMAIL_HAS_TITLE" "1";
    }
}

