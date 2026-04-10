use rustot::shadows::{ShadowPatch, ShadowState};
use rustot_derive::{shadow, shadow_patch};
use serde::{Deserialize, Serialize};

#[test]
fn nested() {
    #[shadow(name = "test", max_payload_size = 256)]
    #[derive(Debug, PartialEq)]
    // #[shadow_all(derive(Clone, Debug, PartialEq))]
    // #[shadow_all(serde(rename_all = "lowercase"))]
    // #[shadow_delta(derive(Deserialize))]
    // #[shadow_reported(derive(Serialize, Default))]
    struct Foo {
        pub bar: u8,

        #[shadow_attr(leaf)]
        #[serde(rename = "desired_rename")]
        pub baz: String,

        pub inner: Inner,

        #[shadow_attr(report_only)]
        #[serde(rename = "report_only_rename")]
        pub report_only: u8,
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct Inner {
        hello: u16,

        #[shadow_attr(report_only, leaf)]
        inner_report: String,
    }

    let mut foo = Foo {
        bar: 56,
        baz: "HelloWorld".to_string(),
        inner: Inner { hello: 1337 },
    };

    ReportedFoo {
        bar: Some(56),
        baz: Some("HelloWorld".to_string()),
        inner: Some(ReportedInner {
            hello: Some(1337),
            inner_report: None,
        }),
        report_only: None,
    };

    let delta = DeltaFoo {
        bar: Some(66),
        baz: None,
        inner: Some(DeltaInner { hello: None }),
    };

    assert_eq!(Foo::NAME, Some("test"));
    assert_eq!(Foo::MAX_PAYLOAD_SIZE, 256);

    assert_eq!(
        ReportedFoo::from(foo.clone()),
        ReportedFoo {
            bar: Some(56),
            baz: Some("HelloWorld".to_string()),
            inner: Some(ReportedInner {
                hello: Some(1337),
                inner_report: None,
            }),
            report_only: None,
        }
    );

    foo.apply_patch(delta);

    assert_eq!(
        foo,
        Foo {
            bar: 66,
            baz: "HelloWorld".to_string(),
            inner: Inner { hello: 1337 }
        }
    );
}

#[test]
fn optionals() {
    #[shadow]
    #[derive(Debug, PartialEq)]
    struct Foo {
        pub bar: u8,

        #[shadow_attr(report_only)]
        pub report_only: Option<u8>,

        #[shadow_attr(report_only)]
        pub report_only_nested: Option<Inner>,
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct Inner {
        hello: u16,

        #[shadow_attr(report_only, leaf)]
        inner_report: String,
    }

    assert_eq!(Foo::NAME, None);
    assert_eq!(Foo::MAX_PAYLOAD_SIZE, 512);

    let mut desired = Foo { bar: 123 };

    desired.apply_patch(DeltaFoo { bar: Some(78) });

    assert_eq!(desired, Foo { bar: 78 });

    let _reported = ReportedFoo {
        bar: Some(56),
        report_only: Some(Some(14)),
        report_only_nested: Some(Some(ReportedInner {
            hello: Some(1337),
            inner_report: None,
        })),
    };
}

#[test]
fn simple_enum() {
    #[shadow]
    #[derive(Debug, PartialEq)]
    struct Foo {
        #[shadow_attr(leaf)]
        pub bar: Either,
    }

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize, Clone)]
    enum Either {
        #[default]
        A,
        B,
    }

    let mut desired = Foo { bar: Either::A };

    let reported = ReportedFoo {
        bar: Some(Either::B),
    };

    desired.apply_patch(DeltaFoo {
        bar: Some(Either::B),
    });

    assert_eq!(ReportedFoo::from(desired), reported);
}

#[test]
fn complex_enum() {
    #[shadow(topic_prefix = "test")]
    #[derive(Debug, PartialEq)]
    struct Foo {
        pub bar: Either,
    }

    #[shadow_patch]
    #[derive(Debug, Default, PartialEq)]
    pub enum Either {
        #[default]
        A(InnerA),
        B(u32),
        C,
        D(InnerA, InnerB),
        E {
            field1: InnerA,
            field2: InnerB,
        },
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct InnerA {
        hello: u16,
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct InnerB {
        baz: i32,
    }

    assert_eq!(Foo::PREFIX, "test");

    let mut desired = Foo {
        bar: Either::A(InnerA { hello: 1337 }),
    };

    let reported = ReportedFoo {
        bar: Some(ReportedEither::D(
            Some(ReportedInnerA { hello: Some(56) }),
            Some(ReportedInnerB { baz: Some(0) }),
        )),
    };

    desired.apply_patch(DeltaFoo {
        bar: Some(DeltaEither::D(Some(DeltaInnerA { hello: Some(56) }), None)),
    });

    assert_eq!(
        desired,
        Foo {
            bar: Either::D(InnerA { hello: 56 }, InnerB::default())
        }
    );
    assert_eq!(ReportedFoo::from(desired), reported);
}

#[test]
fn static_str() {
    #[shadow]
    #[derive(Debug, PartialEq)]
    struct Foo {
        // fails: &'static str,
        #[shadow_attr(report_only, leaf)]
        pub bar: &'static str,

        #[shadow_attr(report_only, leaf)]
        pub baz: Option<&'static str>,
    }

    let _foo = Foo {};

    let _reported = ReportedFoo {
        bar: Some("Hello"),
        baz: Some(Some("HelloBaz")),
    };
}

#[test]
fn manual_reported() {
    #[shadow(name = "manual", reported = ManualReportedFoo)]
    #[derive(Debug, PartialEq)]
    struct Foo {
        pub bar: u8,

        #[shadow_attr(leaf)]
        #[serde(rename = "desired_rename")]
        pub baz: String,

        pub inner: Inner,

        #[shadow_attr(report_only)]
        #[serde(rename = "report_only_rename")]
        pub report_only: u8,
    }
    #[derive(Serialize, Default, Debug, PartialEq)]
    struct ManualReportedFoo {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub bar: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "desired_rename")]
        pub baz: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub inner: Option<<Inner as rustot::shadows::ShadowPatch>::Reported>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "report_only_rename")]
        pub report_only: Option<u8>,
    }

    impl From<Foo> for ManualReportedFoo {
        fn from(v: Foo) -> Self {
            Self {
                bar: Some(v.bar),
                baz: Some(v.baz),
                inner: Some(v.inner.into()),
                ..Default::default()
            }
        }
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct Inner {
        hello: u16,

        #[shadow_attr(report_only, leaf)]
        inner_report: String,
    }

    let mut foo = Foo {
        bar: 56,
        baz: "HelloWorld".to_string(),
        inner: Inner { hello: 1337 },
    };

    ManualReportedFoo {
        bar: Some(56),
        baz: Some("HelloWorld".to_string()),
        inner: Some(ReportedInner {
            hello: Some(1337),
            inner_report: None,
        }),
        report_only: None,
    };

    let delta = DeltaFoo {
        bar: Some(66),
        baz: None,
        inner: Some(DeltaInner { hello: None }),
    };

    assert_eq!(Foo::NAME, Some("manual"));

    assert_eq!(
        ManualReportedFoo::from(foo.clone()),
        ManualReportedFoo {
            bar: Some(56),
            baz: Some("HelloWorld".to_string()),
            inner: Some(ReportedInner {
                hello: Some(1337),
                inner_report: None,
            }),
            report_only: None,
        }
    );

    foo.apply_patch(delta);

    assert_eq!(
        foo,
        Foo {
            bar: 66,
            baz: "HelloWorld".to_string(),
            inner: Inner { hello: 1337 }
        }
    );
}

#[test]
fn enum_leaf() {
    use heapless::String;

    #[shadow_patch]
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub enum LeafField {
        #[default]
        None,

        Inner(#[shadow_attr(leaf)] String<64>),
    }

    // #[shadow_patch]
    // #[derive(Debug, Clone, Default, PartialEq, Eq)]
    // pub enum LeafVariant {
    //     #[default]
    //     None,

    //     #[shadow_attr(leaf)]
    //     Inner(String<64>),
    // }
}

// #[test]
// fn generics() {
//     use heapless::String;

//     #[shadow_patch]
//     #[derive(Debug, Clone)]
//     pub struct Foo<A> {
//         #[shadow_attr(leaf)]
//         pub ssid: String<64>,

//         pub generic: Inner<A>,
//     }

//     #[shadow_patch]
//     #[derive(Debug, Clone)]
//     pub struct Inner<A> {
//         #[shadow_attr(leaf)]
//         a: A,
//     }
// }

#[test]
fn report_only_persist() {
    #[shadow(name = "persist_test")]
    #[derive(Debug, PartialEq)]
    struct Config {
        pub mode: u8,

        pub inner: Inner,

        /// This field is kept in the struct and persisted, but NOT in Delta
        #[shadow_attr(report_only(persist), leaf)]
        pub verified: bool,

        #[shadow_attr(report_only, leaf)]
        pub status: u8,
    }

    #[shadow_patch]
    #[derive(Debug, PartialEq)]
    struct Inner {
        hello: u16,
    }

    // 1. Field exists on the runtime struct
    let config = Config {
        mode: 1,
        inner: Inner { hello: 42 },
        verified: true,
    };
    assert!(config.verified);

    // 2. report_only(persist) field does NOT exist on Delta
    //    (if it did, this wouldn't compile because we'd need to set it)
    let delta = DeltaConfig {
        mode: Some(2),
        inner: None,
    };

    // 3. report_only(persist) field exists on Reported with actual value
    let reported = ReportedConfig::from(config.clone());
    assert_eq!(reported.verified, Some(true)); // persist: gets Some(v.verified)
    assert_eq!(reported.status, None); // plain report_only: gets None
    assert_eq!(reported.mode, Some(1));
    assert_eq!(reported.inner, Some(ReportedInner { hello: Some(42) }));

    // 4. apply_patch does not touch report_only(persist) fields
    let mut config2 = Config {
        mode: 1,
        inner: Inner { hello: 10 },
        verified: true,
    };
    config2.apply_patch(delta);
    assert_eq!(config2.mode, 2); // changed by delta
    assert!(config2.verified); // unchanged -- not in delta

    // 5. Reported struct can set report_only(persist) fields
    let reported_manual = ReportedConfig {
        mode: None,
        inner: None,
        verified: Some(false),
        status: Some(42),
    };
    assert_eq!(reported_manual.verified, Some(false));
}

#[test]
fn report_only_persist_with_nested() {
    /// Test that report_only(persist) works alongside other field types
    #[shadow(name = "mixed")]
    #[derive(Debug, PartialEq)]
    struct Mixed {
        pub normal: u32,

        #[shadow_attr(report_only(persist), leaf)]
        pub local_flag: bool,

        #[shadow_attr(report_only)]
        pub ephemeral: u16,
    }

    // Struct has normal + persist fields, not ephemeral
    let m = Mixed {
        normal: 10,
        local_flag: false,
    };

    // Delta only has normal field
    let delta = DeltaMixed { normal: Some(20) };

    // Reported has all fields
    let reported = ReportedMixed::from(m.clone());
    assert_eq!(reported.normal, Some(10));
    assert_eq!(reported.local_flag, Some(false));
    assert_eq!(reported.ephemeral, None);

    // apply_patch only affects normal fields
    let mut m2 = m;
    m2.apply_patch(delta);
    assert_eq!(m2.normal, 20);
    assert!(!m2.local_flag); // unchanged
}
