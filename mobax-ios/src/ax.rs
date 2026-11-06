//! The accessibility inspector service (`axAuditDaemon`), the one Xcode's
//! Accessibility Inspector drives. A port of go-ios's `ios/accessibility`.
//!
//! It needs no signing and installs nothing. The inspector has a focus, as
//! VoiceOver does: we move it (first, next, previous) and the device reports
//! each element it lands on with `hostInspectorCurrentElementChanged:`. There
//! is no whole-tree call and, as far as measured, no on-screen frame; a full
//! screen costs one round trip per element (~30 ms each on an iPhone 11).
//!
//! Every argument is NSKeyedArchived into the DTX aux section; replies come
//! back as archived payloads. Focus reports are device-to-host calls on the
//! root channel; [Dtx] acknowledges them and keeps them until asked for.

use std::time::Duration;

use idevice::provider::IdeviceProvider;
use plist::Value;

use crate::{
    archive::{dig, unwrap, Arch},
    dtx::{Dtx, Error},
};

const SERVICE: &str = "com.apple.accessibility.axAuditDaemon.remoteserver";
const FOCUS_CHANGED: &str = "hostInspectorCurrentElementChanged:";
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Where [Ax::step] moves the inspector focus.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // the inspector's full set; traversal uses First and Next
pub enum Move {
    Previous = 3,
    Next = 4,
    First = 5,
    Last = 6,
}

/// One element the inspector focused.
#[derive(Clone, Debug)]
pub struct Element {
    /// What VoiceOver would say: label, then value and traits ("Log out, Button").
    pub spoken: String,
    /// The inspector's caption, usually the same as `spoken`.
    pub caption: String,
    /// Opaque handle for [Ax::attribute] and [Ax::activate]; stable while the
    /// element lives, so it also tells when a traversal wrapped around.
    pub handle: Vec<u8>,
    /// Attributes the inspector lists for it (Label, Identifier, AXAction-2010…).
    pub attributes: Vec<String>,
}

impl Element {
    /// The spoken text, or the caption when there is none.
    pub fn text(&self) -> &str {
        if self.spoken.is_empty() {
            &self.caption
        } else {
            &self.spoken
        }
    }

    /// The element's actions, by AX action id (2010 is Activate).
    pub fn actions(&self) -> impl Iterator<Item = &str> {
        self.attributes
            .iter()
            .filter_map(|a| a.strip_prefix("AXAction-"))
    }
}

pub struct Ax {
    dtx: Dtx,
}

impl Ax {
    /// Opens the service and runs the same setup handshake as Xcode (and go-ios):
    /// without it the device never reports focus changes.
    pub async fn connect(provider: &dyn IdeviceProvider) -> Result<Self, String> {
        let socket = crate::device::start_service(provider, SERVICE)
            .await
            .map_err(|e| format!("{SERVICE}: {e}"))?;
        let mut ax = Ax {
            dtx: Dtx::new(socket, REPLY_TIMEOUT),
        };
        ax.handshake().await.map_err(|e| e.to_string())?;
        Ok(ax)
    }

    async fn handshake(&mut self) -> Result<(), Error> {
        self.send(
            "_notifyOfPublishedCapabilities:",
            vec![Arch::dict([
                ("com.apple.private.DTXBlockCompression", Arch::Int(2)),
                ("com.apple.private.DTXConnection", Arch::Int(1)),
            ])],
        )
        .await?;
        self.call("deviceCapabilities", vec![]).await?;
        let api = self
            .call("deviceApiVersion", vec![])
            .await?
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0);
        // API 21 is iOS 15.
        let audit_types = if api >= 21 {
            "deviceAllSupportedAuditTypes"
        } else {
            "deviceAllAuditCaseIDs"
        };
        self.call(audit_types, vec![]).await?;
        self.call("deviceInspectorSupportedEventTypes", vec![])
            .await?;
        self.call("deviceInspectorCanNavWhileMonitoringEvents", vec![])
            .await?;
        self.send("deviceSetAppMonitoringEnabled:", vec![Arch::Bool(true)])
            .await
    }

    /// Points the inspector at the device (rather than one process) and turns
    /// on selection mode, so moves report elements. Returns the element it starts on.
    pub async fn start(&mut self) -> Result<Option<Element>, Error> {
        self.turn_off().await?;
        self.call("deviceAccessibilitySettings", vec![]).await?;
        self.send(
            "deviceInspectorShowIgnoredElements:",
            vec![Arch::Bool(false)],
        )
        .await?;
        self.send("deviceSetAuditTargetPid:", vec![Arch::Int(0)])
            .await?;
        self.send("deviceInspectorFocusOnElement:", vec![Arch::Null])
            .await?;
        let first = self.focus(REPLY_TIMEOUT).await.ok();
        self.send("deviceInspectorPreviewOnElement:", vec![Arch::Null])
            .await?;
        self.send("deviceHighlightIssue:", vec![Arch::dict([])])
            .await?;
        // Monitored event type 2 is selection mode. On iOS 27 it swallows real
        // touches (they move the focus instead), so [Ax::turn_off] before tapping.
        self.send("deviceInspectorSetMonitoredEventType:", vec![Arch::Int(2)])
            .await?;
        self.send("deviceInspectorShowVisuals:", vec![Arch::Bool(true)])
            .await?;
        Ok(first)
    }

    /// Leaves selection mode and hides the inspector's highlight.
    pub async fn turn_off(&mut self) -> Result<(), Error> {
        self.send("deviceInspectorSetMonitoredEventType:", vec![Arch::Int(0)])
            .await?;
        self.send("deviceInspectorFocusOnElement:", vec![Arch::Null])
            .await?;
        let _ = self.focus(Duration::from_secs(1)).await;
        self.send("deviceInspectorPreviewOnElement:", vec![Arch::Null])
            .await?;
        self.send("deviceHighlightIssue:", vec![Arch::dict([])])
            .await?;
        self.send("deviceInspectorShowVisuals:", vec![Arch::Bool(false)])
            .await?;
        self.send("deviceSetAppMonitoringEnabled:", vec![Arch::Bool(false)])
            .await
    }

    /// Moves the focus and returns the element it lands on.
    pub async fn step(&mut self, to: Move) -> Result<Element, Error> {
        let options = Arch::passthrough(Arch::mdict([
            ("allowNonAX", Arch::passthrough(Arch::Bool(false))),
            ("direction", Arch::passthrough(Arch::Int(to as i64))),
            ("includeContainers", Arch::passthrough(Arch::Bool(true))),
        ]));
        self.send("deviceInspectorMoveWithOptions:", vec![options])
            .await?;
        self.focus(REPLY_TIMEOUT).await
    }

    /// Every element from the first, until the focus wraps around or stops
    /// moving, at most `max`.
    pub async fn elements(&mut self, max: usize) -> Result<Vec<Element>, Error> {
        let mut out: Vec<Element> = Vec::new();
        let mut to = Move::First;
        while out.len() < max {
            let e = match self.step(to).await {
                Ok(e) => e,
                Err(Error::Timeout) if !out.is_empty() => break,
                Err(e) => return Err(e),
            };
            if out.iter().any(|seen| seen.handle == e.handle) {
                break;
            }
            out.push(e);
            to = Move::Next;
        }
        Ok(out)
    }

    /// The string value of attribute `name` ("Label", "Identifier", "Hint"…).
    pub async fn attribute(&mut self, el: &Element, name: &str) -> Result<Option<String>, Error> {
        let attr = Arch::typed(
            "AXAuditElementAttribute_v1",
            Arch::passthrough(Arch::mdict([(
                "AttributeNameValue_v1",
                Arch::passthrough(Arch::str(name)),
            )])),
        );
        let reply = self
            .call(
                "deviceElement:valueForAttribute:",
                vec![element_arg(el), attr],
            )
            .await?;
        Ok(reply
            .as_ref()
            .map(unwrap)
            .and_then(|v| v.as_string())
            .map(str::to_string))
    }

    /// Performs the element's default action (AXAction-2010, "Activate"), what
    /// a VoiceOver double tap does. An element that is only static text inside
    /// a tappable cell accepts this and does nothing.
    pub async fn activate(&mut self, el: &Element) -> Result<(), Error> {
        let flag = |b| Arch::passthrough(Arch::Bool(b));
        let attr = Arch::typed(
            "AXAuditElementAttribute_v1",
            Arch::passthrough(Arch::mdict([
                (
                    "AttributeNameValue_v1",
                    Arch::passthrough(Arch::str("AXAction-2010")),
                ),
                (
                    "HumanReadableNameValue_v1",
                    Arch::passthrough(Arch::str("Activate")),
                ),
                ("PerformsActionValue_v1", flag(true)),
                ("SettableValue_v1", flag(false)),
                ("DisplayAsTree_v1", flag(false)),
                ("DisplayInlineValue_v1", flag(false)),
                ("IsInternal_v1", flag(false)),
                ("ValueTypeValue_v1", Arch::passthrough(Arch::Int(1))),
            ])),
        );
        self.call(
            "deviceElement:performAction:withValue:",
            vec![element_arg(el), attr, Arch::mdict([])],
        )
        .await?;
        Ok(())
    }

    /// Waits for the next focus report.
    async fn focus(&mut self, timeout: Duration) -> Result<Element, Error> {
        let msg = self
            .dtx
            .wait_call(timeout, |m| m.selector() == Some(FOCUS_CHANGED))
            .await?;
        // Other device calls (monitored events, settings changes) are not used;
        // drop them so the queue stays small.
        self.dtx
            .incoming
            .retain(|m| m.selector() == Some(FOCUS_CHANGED));
        let payload = msg
            .args()
            .next()
            .ok_or_else(|| bad("focus report without an element"))?;
        parse_focus(&payload).ok_or_else(|| bad("focus report in an unknown shape"))
    }

    /// Calls `sel` without waiting.
    async fn send(&mut self, sel: &str, args: Vec<Arch>) -> Result<(), Error> {
        self.dtx.send(0, sel, &args).await
    }

    /// Calls `sel` and returns its reply's payload.
    async fn call(&mut self, sel: &str, args: Vec<Arch>) -> Result<Option<Value>, Error> {
        self.dtx.call(0, sel, &args).await
    }
}

fn bad(what: &str) -> Error {
    Error::Protocol(what.into())
}

fn element_arg(el: &Element) -> Arch {
    Arch::typed(
        "AXAuditElement_v1",
        Arch::passthrough(Arch::mdict([(
            "PlatformElementValue_v1",
            Arch::passthrough(Arch::Data(el.handle.clone())),
        )])),
    )
}

/// Reads an `AXAuditInspectorFocus_v1` report.
fn parse_focus(v: &Value) -> Option<Element> {
    let inner = dig(v, &["Value", "Value"])?;
    let text = |key: &str| {
        dig(inner, &[key])
            .map(unwrap)
            .and_then(|s| s.as_string())
            .unwrap_or("")
            .to_string()
    };
    let handle = dig(
        inner,
        &[
            "ElementValue_v1",
            "Value",
            "Value",
            "PlatformElementValue_v1",
        ],
    )
    .map(unwrap)
    .and_then(|d| d.as_data())?
    .to_vec();
    let mut attributes = Vec::new();
    if let Some(sections) = dig(inner, &["InspectorSectionsValue_v1"]) {
        attribute_names(sections, &mut attributes);
    }
    Some(Element {
        spoken: text("SpokenDescriptionValue_v1"),
        caption: text("CaptionTextValue_v1"),
        handle,
        attributes,
    })
}

fn attribute_names(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Dictionary(d) => {
            if let Some(name) = d
                .get("AttributeNameValue_v1")
                .map(unwrap)
                .and_then(|n| n.as_string())
            {
                out.push(name.to_string());
            }
            d.values().for_each(|x| attribute_names(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| attribute_names(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{archive, unarchive};

    /// The shape of a focus report as recorded from an iPhone 11 on iOS 18.5
    /// (go-ios's decoder, the same NSKeyedArchive), trimmed to two attributes.
    fn report() -> Value {
        let attr = |name: &str| {
            Arch::typed(
                "AXAuditElementAttribute_v1",
                Arch::passthrough(Arch::mdict([
                    ("AttributeNameValue_v1", Arch::passthrough(Arch::str(name))),
                    ("ValueTypeValue_v1", Arch::passthrough(Arch::Int(2))),
                ])),
            )
        };
        let section = Arch::typed(
            "AXAuditInspectorSection_v1",
            Arch::passthrough(Arch::mdict([(
                "ElementAttributesValue_v1",
                Arch::passthrough(Arch::mdict([
                    ("0", attr("Label")),
                    ("1", attr("AXAction-2010")),
                ])),
            )])),
        );
        let focus = Arch::typed(
            "AXAuditInspectorFocus_v1",
            Arch::passthrough(Arch::mdict([
                (
                    "CaptionTextValue_v1",
                    Arch::passthrough(Arch::str("로그아웃, 버튼")),
                ),
                (
                    "SpokenDescriptionValue_v1",
                    Arch::passthrough(Arch::str("로그아웃, 버튼")),
                ),
                (
                    "ElementValue_v1",
                    Arch::typed(
                        "AXAuditElement_v1",
                        Arch::passthrough(Arch::mdict([(
                            "PlatformElementValue_v1",
                            Arch::passthrough(Arch::Data(vec![0xfc, 0x0f, 0, 0, 0x9b, 0xd1])),
                        )])),
                    ),
                ),
                (
                    "InspectorSectionsValue_v1",
                    Arch::passthrough(Arch::mdict([("0", section)])),
                ),
            ])),
        );
        unarchive(&archive(&focus)).unwrap()
    }

    #[test]
    fn reads_a_focus_report() {
        let e = parse_focus(&report()).expect("parses");
        assert_eq!(e.spoken, "로그아웃, 버튼");
        assert_eq!(e.handle, vec![0xfc, 0x0f, 0, 0, 0x9b, 0xd1]);
        assert!(e.attributes.contains(&"Label".to_string()));
        assert_eq!(e.actions().collect::<Vec<_>>(), vec!["2010"]);
    }

    #[test]
    fn a_report_without_an_element_is_rejected() {
        let v = unarchive(&archive(&Arch::typed(
            "AXAuditInspectorFocus_v1",
            Arch::passthrough(Arch::mdict([(
                "CaptionTextValue_v1",
                Arch::passthrough(Arch::str("x")),
            )])),
        )))
        .unwrap();
        assert!(parse_focus(&v).is_none());
    }
}
