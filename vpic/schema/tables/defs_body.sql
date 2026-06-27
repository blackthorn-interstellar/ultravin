CREATE TABLE vpic.defs_body (
    id smallint NOT NULL,
    def character varying(500),
    body_type character varying(2),
    from_year smallint DEFAULT 1994 NOT NULL,
    to_year smallint,
    mode smallint NOT NULL
);
