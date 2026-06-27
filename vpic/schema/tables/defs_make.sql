CREATE TABLE vpic.defs_make (
    id smallint NOT NULL,
    def character varying(35),
    ncic_code character varying(10),
    make_type character varying(1),
    from_year smallint DEFAULT 1994 NOT NULL,
    to_year smallint,
    mode smallint NOT NULL
);
