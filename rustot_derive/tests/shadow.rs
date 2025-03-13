use rustot::shadows::{ShadowPatch, ShadowState};
use rustot_derive::{shadow, shadow_patch};
use serde::{Deserialize, Serialize};

#[test]
fn nested() {
    #[shadow(name = "test", max_payload_size = 256)]
    #[derive(Debug, PartialEq)]
    #[serde(rename_all = "lowercase")]
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
