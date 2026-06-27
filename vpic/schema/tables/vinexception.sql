CREATE TABLE vpic.vinexception (
    id integer NOT NULL,
    vin character varying(17) NOT NULL,
    checkdigit boolean DEFAULT false NOT NULL,
    createdon timestamp without time zone,
    updatedon timestamp without time zone
);
