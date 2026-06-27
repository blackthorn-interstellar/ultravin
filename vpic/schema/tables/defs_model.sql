CREATE TABLE vpic.defs_model (
    make smallint NOT NULL,
    id smallint NOT NULL,
    def character varying(300),
    model_type character varying(2),
    includes character varying,
    from_year smallint DEFAULT 1994 NOT NULL,
    to_year smallint,
    mode smallint NOT NULL
);
